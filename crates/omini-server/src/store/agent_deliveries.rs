use super::Store;
use super::context::next_llm_ordinal;
use super::messages::{NewUiJson, insert_ui_json};
use crate::store::StoreError;
use jiff::Timestamp;
use omini_config::project::ThreadDir;
use omini_domain::conversation::{AgentMessage, ConversationEntry, SystemEvent};
use omini_entity::{
    AgentTask, AgentTaskDelivery, BackgroundTask, DeliveryStatus, LlmMessage, MessageKind,
    SourceKind, Thread, cleanup_created_files, prepare_blocks,
};
use omini_model::message::Role;
use omini_runtime_contract::thread_domain::ClientMessage;
use omini_runtime_contract::thread_domain::{AgentTaskResult, DeliveryKey};

impl Store {
    pub async fn projected_delivery_keys(
        &self,
        owner_thread_id: &str,
    ) -> Result<Vec<DeliveryKey>, StoreError> {
        let mut db = self.conn();
        let rows = AgentTaskDelivery::filter_by_owner_thread_id(owner_thread_id)
            .exec(&mut db)
            .await?;
        Ok(rows
            .into_iter()
            .map(|row| DeliveryKey {
                task_id: row.task_id,
                source_kind: row.source_kind.as_str().to_string(),
                source_key: row.source_key,
            })
            .collect())
    }

    /// 返回是否首次登记;已有相同来源和内容的请求不应再次入队。
    pub async fn enqueue_client_message(
        &self,
        task_id: &str,
        owner_thread_id: &str,
        agent_thread_id: &str,
        message: &ClientMessage,
        thread_dir: &ThreadDir,
    ) -> Result<bool, StoreError> {
        let source_key =
            DeliveryKey::from_client(task_id, &message.client_id, &message.client_echo_id)
                .source_key;
        self.enqueue_delivery(DeliveryRegistration {
            task_id,
            owner_thread_id,
            agent_thread_id,
            source_kind: SourceKind::Client,
            source_key: &source_key,
            payload: serde_json::to_value(&message.input)?,
            entry: ConversationEntry::UserInput(message.input.clone()),
            thread_dir,
        })
        .await
    }

    /// 查询客户端投递登记(载荷与状态);用于幂等回显比对。
    pub async fn client_delivery(
        &self,
        task_id: &str,
        message: &ClientMessage,
    ) -> Result<Option<(serde_json::Value, DeliveryStatus)>, StoreError> {
        let source_key =
            DeliveryKey::from_client(task_id, &message.client_id, &message.client_echo_id)
                .source_key;
        let mut db = self.conn();
        Ok(
            match AgentTaskDelivery::filter(
                AgentTaskDelivery::fields()
                    .task_id()
                    .eq(task_id)
                    .and(
                        AgentTaskDelivery::fields()
                            .source_kind()
                            .eq(SourceKind::Client),
                    )
                    .and(AgentTaskDelivery::fields().source_key().eq(source_key)),
            )
            .first()
            .exec(&mut db)
            .await?
            {
                Some(row) => Some((row.payload.clone(), row.status)),
                None => None,
            },
        )
    }

    pub async fn fail_client_message(
        &self,
        task_id: &str,
        message: &ClientMessage,
        reason: &str,
    ) -> Result<u32, StoreError> {
        let key = DeliveryKey::from_client(task_id, &message.client_id, &message.client_echo_id);
        self.settle_deliveries(
            Some(task_id),
            Some(SourceKind::Client),
            Some(&key.source_key),
            reason,
        )
        .await
        .map(|counts| counts.first().map_or(0, |(_, count)| *count))
    }

    pub async fn enqueue_agent_message(
        &self,
        task_id: &str,
        owner_thread_id: &str,
        agent_thread_id: &str,
        message: &AgentMessage,
        thread_dir: &ThreadDir,
    ) -> Result<bool, StoreError> {
        let source_key =
            DeliveryKey::from_agent(task_id, &message.source_run_id, &message.tool_use_id)
                .source_key;
        self.enqueue_delivery(DeliveryRegistration {
            task_id,
            owner_thread_id,
            agent_thread_id,
            source_kind: SourceKind::Agent,
            source_key: &source_key,
            payload: serde_json::to_value(message)?,
            entry: ConversationEntry::SystemEvent(SystemEvent::AgentMessage(message.clone())),
            thread_dir,
        })
        .await
    }

    /// 消息入队登记 + 子会话展示历史写入的公共路径。
    /// 首次判定依赖复合主键 upsert 的 or_ignore(None 即已存在);
    /// 已存在时校验载荷一致且未被判不可投递。
    async fn enqueue_delivery(
        &self,
        registration: DeliveryRegistration<'_>,
    ) -> Result<bool, StoreError> {
        let DeliveryRegistration {
            task_id,
            owner_thread_id,
            agent_thread_id,
            source_kind,
            source_key,
            payload,
            entry,
            thread_dir,
        } = registration;
        let mut conn = self.conn();
        let mut tx = conn.transaction().await?;
        let inserted = AgentTaskDelivery::upsert_by_task_id_and_source_kind_and_source_key(
            task_id.to_string(),
            source_kind,
            source_key.to_string(),
        )
        .owner_thread_id(owner_thread_id.to_string())
        .agent_thread_id(agent_thread_id.to_string())
        .payload(payload.clone())
        .status(DeliveryStatus::Pending)
        .or_ignore()
        .exec(&mut tx)
        .await?
        .is_some();
        if inserted {
            // 子会话展示历史与登记同事务写入。
            insert_ui_json(
                NewUiJson {
                    thread_id: agent_thread_id,
                    role: Role::User,
                    model_ref: None,
                    content: &serde_json::to_string(&entry)?,
                    kind: MessageKind::ConversationEntry,
                    created_at: Timestamp::now(),
                },
                thread_dir,
                &mut tx,
            )
            .await?;
        } else {
            let existing = AgentTaskDelivery::filter(
                AgentTaskDelivery::fields()
                    .task_id()
                    .eq(task_id)
                    .and(AgentTaskDelivery::fields().source_kind().eq(source_kind))
                    .and(AgentTaskDelivery::fields().source_key().eq(source_key)),
            )
            .first()
            .exec(&mut tx)
            .await?
            .ok_or_else(|| StoreError::MissingRow(format!("delivery '{source_key}'")))?;
            if existing.payload != payload {
                return Err(StoreError::InvalidData(
                    "delivery source key was reused with different content".to_string(),
                ));
            }
            if existing.status == DeliveryStatus::Failed {
                return Err(StoreError::InvalidData(
                    "delivery was already marked undeliverable".to_string(),
                ));
            }
        }
        tx.commit().await?;
        Ok(inserted)
    }

    /// 模型历史和投递状态必须原子提交;重复注入不追加第二条模型消息。
    pub async fn inject_task_message(
        &self,
        agent_thread_id: &str,
        key: &DeliveryKey,
        model_message: &omini_model::message::Message,
        thread_dir: &ThreadDir,
    ) -> Result<(), StoreError> {
        let mut conn = self.conn();
        let mut tx = conn.transaction().await?;
        let source_kind = SourceKind::parse(&key.source_kind)?;
        let delivery = AgentTaskDelivery::filter(
            AgentTaskDelivery::fields()
                .task_id()
                .eq(&key.task_id)
                .and(AgentTaskDelivery::fields().source_kind().eq(source_kind))
                .and(AgentTaskDelivery::fields().source_key().eq(&key.source_key)),
        )
        .first()
        .exec(&mut tx)
        .await?
        .ok_or_else(|| StoreError::MissingRow(format!("delivery '{}'", key.source_key)))?;
        if delivery.agent_thread_id != agent_thread_id {
            return Err(StoreError::MissingRow(format!(
                "delivery '{}' for thread '{agent_thread_id}'",
                key.source_key
            )));
        }
        match delivery.status {
            DeliveryStatus::Injected => return Ok(()),
            DeliveryStatus::Failed => {
                return Err(StoreError::InvalidData(
                    "task message cannot be injected after delivery failed".to_string(),
                ));
            }
            DeliveryStatus::Pending => {}
        }
        let thread = Thread::filter_by_id(agent_thread_id)
            .first()
            .exec(&mut tx)
            .await?
            .ok_or_else(|| StoreError::MissingRow(format!("thread '{agent_thread_id}'")))?;
        let version = thread.llm_context_version;
        let ordinal = next_llm_ordinal(&mut tx, agent_thread_id, version).await?;
        // 模型上下文同样经 sidecar 路径落盘,与 append_llm_message 一致。
        let prepared = prepare_blocks(&model_message.content, thread_dir)?;
        let llm_json = serde_json::to_string(&prepared.values)?;
        let result: Result<(), StoreError> = async {
            toasty::create!(LlmMessage {
                thread_id: agent_thread_id.to_string(),
                context_version: version,
                ordinal,
                role: model_message.role,
                content: llm_json,
                created_at: Timestamp::now(),
            })
            .exec(&mut tx)
            .await?;
            let mut delivery = delivery;
            toasty::update!(delivery {
                status: DeliveryStatus::Injected,
                updated_at: Timestamp::now(),
            })
            .exec(&mut tx)
            .await?;
            tx.commit().await?;
            Ok(())
        }
        .await;
        if result.is_err() {
            cleanup_created_files(&prepared.created_files);
        }
        result
    }

    pub async fn fail_task_messages(&self, task_id: &str, reason: &str) -> Result<u32, StoreError> {
        let counts = self
            .settle_deliveries(Some(task_id), None, None, reason)
            .await?;
        Ok(counts.first().map_or(0, |(_, count)| *count))
    }

    pub async fn fail_pending_deliveries(&self, reason: &str) -> Result<(), StoreError> {
        self.settle_deliveries(None, None, None, reason)
            .await
            .map(|_| ())
    }

    /// 结算待投递记录并返回每个任务累计失败数;终态结果据此给出摘要。
    /// 过滤条件按需组合,全部行为发生在单事务内。
    pub(super) async fn settle_deliveries(
        &self,
        only_task_id: Option<&str>,
        source_kind: Option<SourceKind>,
        source_key: Option<&str>,
        reason: &str,
    ) -> Result<Vec<(String, u32)>, StoreError> {
        let now = Timestamp::now();
        let mut conn = self.conn();
        let mut tx = conn.transaction().await?;

        let mut filter = AgentTaskDelivery::fields()
            .status()
            .eq(DeliveryStatus::Pending);
        if let Some(task_id) = only_task_id {
            filter = filter.and(AgentTaskDelivery::fields().task_id().eq(task_id));
        }
        if let Some(kind) = source_kind {
            filter = filter.and(AgentTaskDelivery::fields().source_kind().eq(kind));
        }
        if let Some(key) = source_key {
            filter = filter.and(AgentTaskDelivery::fields().source_key().eq(key));
        }
        let mut pending = AgentTaskDelivery::filter(filter).exec(&mut tx).await?;
        let mut tasks: Vec<String> = pending.iter().map(|row| row.task_id.clone()).collect();
        // 等价 SELECT DISTINCT:dedup 只去相邻重复,先排序再去重。
        tasks.sort();
        tasks.dedup();
        for row in &mut pending {
            toasty::update!(row {
                status: DeliveryStatus::Failed,
                failure_reason: Some(reason.to_string()),
                updated_at: now,
            })
            .exec(&mut tx)
            .await?;
        }
        if let Some(task_id) = only_task_id
            && !tasks.iter().any(|id| id == task_id)
        {
            tasks.push(task_id.to_string());
        }
        let mut counts = Vec::with_capacity(tasks.len());
        for task_id in &tasks {
            let count = AgentTaskDelivery::filter(
                AgentTaskDelivery::fields()
                    .task_id()
                    .eq(task_id.clone())
                    .and(
                        AgentTaskDelivery::fields()
                            .status()
                            .eq(DeliveryStatus::Failed),
                    ),
            )
            .count()
            .exec(&mut tx)
            .await?;
            let count = u32::try_from(count)
                .map_err(|_| StoreError::InvalidData("delivery count overflow".to_string()))?;
            if only_task_id.is_none() && count > 0 {
                // 全局结算(启动恢复)把失败数写入任务结果摘要。
                let existing = AgentTask::filter_by_task_id(task_id)
                    .first()
                    .exec(&mut tx)
                    .await?;
                let mut result = existing
                    .and_then(|task| task.result)
                    .map(|result| result.0)
                    .unwrap_or(AgentTaskResult {
                        output: None,
                        error: None,
                        warnings: Vec::new(),
                        undelivered_messages: None,
                    });
                result.undelivered_messages = Some(count);
                if let Some(mut task) = AgentTask::filter_by_task_id(task_id)
                    .first()
                    .exec(&mut tx)
                    .await?
                {
                    toasty::update!(task {
                        result: Some(toasty::stmt::Json(result))
                    })
                    .exec(&mut tx)
                    .await?;
                }
                if let Some(mut background) = BackgroundTask::filter_by_task_id(task_id)
                    .first()
                    .exec(&mut tx)
                    .await?
                {
                    toasty::update!(background {
                        result_summary: Some(format!("{count} 条消息未进入子 Agent 模型上下文"),),
                    })
                    .exec(&mut tx)
                    .await?;
                }
            }
            counts.push((task_id.clone(), count));
        }
        tx.commit().await?;
        Ok(counts)
    }
}

/// 一次投递登记的全部输入:来源键、载荷、登记成功时写入子线程的会话条目
/// 与该线程目录(展示历史走 sidecar 路径)。
struct DeliveryRegistration<'a> {
    task_id: &'a str,
    owner_thread_id: &'a str,
    agent_thread_id: &'a str,
    source_kind: SourceKind,
    source_key: &'a str,
    payload: serde_json::Value,
    entry: ConversationEntry,
    thread_dir: &'a ThreadDir,
}
