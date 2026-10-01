use super::Store;
use crate::store::StoreError;
use jiff::Timestamp;
use omini_config::project::ThreadDir;
use omini_domain::conversation::ConversationEntry;
use omini_entity::{
    Message, MessageKind, PreparedUiContent, finish_prepared_write, prepare_blocks,
    prepare_ui_content,
};
use omini_model::message::{ContentBlock, Role};

/// 待写入的用户可见消息:blocks 经 sidecar 机制落盘后序列化。
pub struct NewMessage {
    pub thread_id: String,
    pub role: Role,
    pub model_ref: Option<String>,
    pub blocks: Vec<ContentBlock>,
    pub kind: MessageKind,
    pub created_at: Timestamp,
}

/// 一次子会话消息提交，按执行投影决定写入模型历史、展示历史或两者。
pub struct AgentMessageCommit<'a> {
    pub thread_id: &'a str,
    pub message: &'a omini_model::message::Message,
    pub model_ref: Option<&'a str>,
    pub persist_llm_history: bool,
    pub display_in_ui: bool,
}

impl Store {
    /// 原子受理新运行：展示输入与初始运行记录同时提交，失败时一起回滚。
    /// sidecar 由本操作拥有，只有事务成功后才保留本次创建的文件。
    pub async fn accept_run_input(
        &self,
        run: &omini_domain::agent_run::AgentRunSnapshot,
        input: &omini_domain::conversation::UserInput,
        thread_dir: &ThreadDir,
    ) -> Result<(), StoreError> {
        let content = serde_json::to_string(&ConversationEntry::UserInput(input.clone()))?;
        let PreparedUiContent {
            value,
            created_files,
        } = prepare_ui_content(&content, thread_dir)?;
        let result: Result<(), toasty::Error> = async {
            let mut db = self.conn();
            let mut tx = db.transaction().await?;
            toasty::create!(Message {
                thread_id: run.thread_id.clone(),
                role: Role::User,
                model_ref: None,
                content: value,
                kind: MessageKind::ConversationEntry,
                created_at: run.created_at,
            })
            .exec(&mut tx)
            .await?;
            toasty::create!(omini_entity::AgentRun {
                id: run.id.clone(),
                thread_id: run.thread_id.clone(),
                parent_run_id: run.parent_run_id.clone(),
                status: run.status,
                created_at: run.created_at,
                started_at: run.started_at,
                finished_at: run.finished_at,
                total_tokens: run.total_tokens,
                archived_at: run.archived_at,
            })
            .exec(&mut tx)
            .await?;
            tx.commit().await?;
            Ok(())
        }
        .await;
        finish_prepared_write(result, &created_files)
    }

    /// 子会话的展示历史与模型历史一次提交，后续写入失败时一起回滚。
    pub async fn persist_agent_message(
        &self,
        commit: AgentMessageCommit<'_>,
        thread_dir: &ThreadDir,
    ) -> Result<(), StoreError> {
        let AgentMessageCommit {
            thread_id,
            message,
            model_ref,
            persist_llm_history,
            display_in_ui,
        } = commit;
        let ui_json = if display_in_ui {
            crate::conversation::entry_from_model_message(message.clone())
                .map(|entry| serde_json::to_string(&entry))
                .transpose()?
        } else {
            None
        };
        let mut created_files = Vec::new();
        let result: Result<(), StoreError> = async {
            let ui_content = if let Some(json) = ui_json {
                let prepared = prepare_ui_content(&json, thread_dir)?;
                created_files.extend(prepared.created_files);
                Some(prepared.value)
            } else {
                None
            };
            let llm_content = if persist_llm_history {
                let prepared = prepare_blocks(&message.content, thread_dir)?;
                created_files.extend(prepared.created_files);
                Some(serde_json::to_string(&prepared.values)?)
            } else {
                None
            };
            let now = Timestamp::now();
            let mut conn = self.conn();
            let mut tx = conn.transaction().await?;
            if let Some(content) = ui_content {
                toasty::create!(Message {
                    thread_id: thread_id.to_string(),
                    role: message.role,
                    model_ref: model_ref.map(ToString::to_string),
                    content,
                    kind: MessageKind::ConversationEntry,
                    created_at: now,
                })
                .exec(&mut tx)
                .await?;
            }
            if let Some(content) = llm_content {
                let version =
                    crate::store::context::current_context_version(&mut tx, thread_id).await?;
                let ordinal =
                    crate::store::context::next_llm_ordinal(&mut tx, thread_id, version).await?;
                toasty::create!(omini_entity::LlmMessage {
                    thread_id: thread_id.to_string(),
                    context_version: version,
                    ordinal,
                    role: message.role,
                    content,
                    created_at: now,
                })
                .exec(&mut tx)
                .await?;
            }
            tx.commit().await?;
            Ok(())
        }
        .await;
        if result.is_err() {
            omini_entity::cleanup_created_files(&created_files);
        }
        result
    }

    pub async fn insert_message(
        &self,
        msg: &NewMessage,
        thread_dir: &ThreadDir,
    ) -> Result<(), StoreError> {
        let prepared = prepare_blocks(&msg.blocks, thread_dir)?;
        let blocks_json = serde_json::to_string(&prepared.values)?;
        let mut db = self.conn();
        let result = toasty::create!(Message {
            thread_id: msg.thread_id.clone(),
            role: msg.role,
            model_ref: msg.model_ref.clone(),
            content: blocks_json,
            kind: msg.kind,
            created_at: msg.created_at,
        })
        .exec(&mut db)
        .await
        .map(|_| ());
        finish_prepared_write(result, &prepared.created_files)
    }

    pub async fn insert_user_input(
        &self,
        thread_id: &str,
        input: &omini_domain::conversation::UserInput,
        created_at: Timestamp,
        thread_dir: &ThreadDir,
    ) -> Result<(), StoreError> {
        self.insert_conversation_entry(
            thread_id,
            &ConversationEntry::UserInput(input.clone()),
            Role::User,
            None,
            created_at,
            thread_dir,
        )
        .await
    }

    pub async fn insert_conversation_entry(
        &self,
        thread_id: &str,
        entry: &ConversationEntry,
        role: Role,
        model_ref: Option<&str>,
        created_at: Timestamp,
        thread_dir: &ThreadDir,
    ) -> Result<(), StoreError> {
        insert_ui_json(
            NewUiJson {
                thread_id,
                role,
                model_ref,
                content: &serde_json::to_string(entry)?,
                kind: MessageKind::ConversationEntry,
                created_at,
            },
            thread_dir,
            &mut self.conn(),
        )
        .await
    }

    pub async fn insert_plan_message(
        &self,
        thread_id: &str,
        plan: &omini_domain::conversation::ProposedPlan,
        model_ref: &str,
        thread_dir: &ThreadDir,
    ) -> Result<(), StoreError> {
        insert_ui_json(
            NewUiJson {
                thread_id,
                role: Role::Assistant,
                model_ref: Some(model_ref),
                content: &serde_json::to_string(&ConversationEntry::SystemEvent(
                    omini_domain::conversation::SystemEvent::Plan(plan.clone()),
                ))?,
                kind: MessageKind::ConversationEntry,
                created_at: plan.created_at,
            },
            thread_dir,
            &mut self.conn(),
        )
        .await
    }

    pub async fn insert_compact_summary_message(
        &self,
        thread_id: &str,
        summary: &omini_domain::conversation::CompactionSummary,
        model_ref: &str,
        thread_dir: &ThreadDir,
    ) -> Result<(), StoreError> {
        insert_ui_json(
            NewUiJson {
                thread_id,
                role: Role::Assistant,
                model_ref: Some(model_ref),
                content: &serde_json::to_string(&ConversationEntry::SystemEvent(
                    omini_domain::conversation::SystemEvent::Summary(summary.clone()),
                ))?,
                kind: MessageKind::ConversationEntry,
                created_at: summary.created_at,
            },
            thread_dir,
            &mut self.conn(),
        )
        .await
    }

    pub async fn get_messages(&self, thread_id: &str) -> Result<Vec<Message>, StoreError> {
        let mut db = self.conn();
        Ok(Message::filter_by_thread_id(thread_id)
            .order_by(Message::fields().id().asc())
            .exec(&mut db)
            .await?)
    }

    pub async fn get_first_message_text(&self, thread_id: &str) -> Result<String, StoreError> {
        let mut db = self.conn();
        let first = Message::filter_by_thread_id(thread_id)
            .order_by(Message::fields().id().asc())
            .first()
            .exec(&mut db)
            .await?;
        Ok(first
            .map(|message| extract_message_text(&message.content))
            .unwrap_or_default())
    }
}

/// 会话条目直接序列化写入的统一入参。
pub(super) struct NewUiJson<'a> {
    pub(super) thread_id: &'a str,
    pub(super) role: Role,
    pub(super) model_ref: Option<&'a str>,
    pub(super) content: &'a str,
    pub(super) kind: MessageKind,
    pub(super) created_at: Timestamp,
}

/// UI JSON 消息写入的公共路径:超阈值内容先落 sidecar 再插行,
/// 插入失败时清理本次落盘文件。接受任意执行器,可在事务内复用。
pub(super) async fn insert_ui_json(
    row: NewUiJson<'_>,
    thread_dir: &ThreadDir,
    db: &mut impl toasty::db::Executor,
) -> Result<(), StoreError> {
    let PreparedUiContent {
        value,
        created_files,
    } = prepare_ui_content(row.content, thread_dir)?;
    let result = toasty::create!(Message {
        thread_id: row.thread_id.to_string(),
        role: row.role,
        model_ref: row.model_ref.map(ToString::to_string),
        content: value,
        kind: row.kind,
        created_at: row.created_at,
    })
    .exec(db)
    .await
    .map(|_| ());
    finish_prepared_write(result, &created_files)
}

/// 提取消息文本用于首条消息标题等场景;解析失败返回空串。
pub(super) fn extract_message_text(content_json: &str) -> String {
    if let Ok(entry) = serde_json::from_str::<ConversationEntry>(content_json) {
        let text = match entry {
            ConversationEntry::UserInput(input) => input
                .parts
                .iter()
                .filter_map(|part| match part {
                    omini_domain::input::InputPart::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<String>(),
            ConversationEntry::AssistantMessage(output) => output
                .blocks
                .iter()
                .filter_map(|block| match block {
                    omini_domain::conversation::AssistantMessageBlock::Text { text } => {
                        Some(text.as_str())
                    }
                    _ => None,
                })
                .collect::<String>(),
            ConversationEntry::SystemEvent(omini_domain::conversation::SystemEvent::Plan(plan)) => {
                plan.markdown
            }
            ConversationEntry::SystemEvent(omini_domain::conversation::SystemEvent::Summary(
                summary,
            )) => summary.markdown,
            ConversationEntry::SystemEvent(
                omini_domain::conversation::SystemEvent::TaskNotification(_),
            ) => String::new(),
            ConversationEntry::SystemEvent(
                omini_domain::conversation::SystemEvent::AgentMessage(message),
            ) => message.text,
            ConversationEntry::SystemEvent(
                omini_domain::conversation::SystemEvent::ToolResults { results },
            ) => results
                .iter()
                .map(|result| result.content.as_str())
                .collect::<Vec<_>>()
                .join(" "),
        };
        return text.replace('\n', " ").replace('\r', "");
    }
    serde_json::from_str::<Vec<serde_json::Value>>(content_json)
        .unwrap_or_default()
        .iter()
        .filter(|value| value.get("type").and_then(serde_json::Value::as_str) == Some("text"))
        .filter_map(|value| value.get("text").and_then(serde_json::Value::as_str))
        .map(|text| text.replace('\n', " ").replace('\r', ""))
        .collect::<Vec<_>>()
        .join(" ")
}
