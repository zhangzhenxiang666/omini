use super::Store;
use super::context::next_llm_ordinal;
use super::messages::{NewUiJson, insert_ui_json};
use crate::store::StoreError;
use jiff::Timestamp;
use omini_config::project::ThreadDir;
use omini_domain::agent_run::AgentRunStatus;
use omini_domain::conversation::{ConversationEntry, UserInput};
use omini_domain::input::{InputPart, UserInputIntent};
use omini_domain::task::TaskStatus;
use omini_entity::{
    AgentRun, AgentTask, BackgroundTask, LlmMessage, Message, MessageKind, Thread,
    thread_from_runtime,
};
use omini_model::message::Role;
use omini_runtime_contract::persistence::ThreadRecord;
use omini_runtime_contract::thread_domain::{AgentTaskInfo, AgentTaskResult};

impl Store {
    /// 创建子 Agent 任务:子线程、任务投影、Run、初始 UI 消息与模型上下文
    /// 必须原子落库,保持与原事务内 7 条 INSERT 一一对应。
    pub async fn create_agent_task(
        &self,
        project_id: &str,
        task: &AgentTaskInfo,
        thread: &ThreadRecord,
        initial_message: &omini_model::message::Message,
    ) -> Result<(), StoreError> {
        let thread = thread_from_runtime(project_id, thread)?;
        let initial_content = serde_json::to_string(&initial_message.content)?;
        let initial_prompt = initial_message
            .content
            .iter()
            .filter_map(|block| match block {
                omini_model::message::ContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect::<String>();
        let initial_entry = serde_json::to_string(&ConversationEntry::UserInput(UserInput {
            intent: UserInputIntent::Message,
            parts: vec![InputPart::Text {
                text: initial_prompt,
            }],
            attachments: Vec::new(),
        }))?;
        let mut conn = self.conn();
        let mut tx = conn.transaction().await?;
        // 引用完整性由本层在创建边界保证(模型 schema 无数据库外键):
        // owner 线程与显式给出的父线程/父任务/父 Run 必须存在,缺失即整体回滚,
        // 与旧 schema 的外键拒绝语义一致。
        ensure_thread_exists(&mut tx, &task.owner_thread_id).await?;
        ensure_thread_exists(&mut tx, &task.parent_thread_id).await?;
        if let Some(parent_task_id) = &task.parent_task_id {
            ensure_task_exists(&mut tx, parent_task_id).await?;
        }
        if let Some(parent_run_id) = &task.parent_run_id {
            ensure_run_exists(&mut tx, parent_run_id).await?;
        }
        toasty::create!(Thread {
            id: thread.id.clone(),
            project_id: thread.project_id.clone(),
            parent_thread_id: thread.parent_thread_id.clone(),
            spawn_tool_use_id: thread.spawn_tool_use_id.clone(),
            thread_type: thread.thread_type,
            agent_label: thread.agent_label.clone(),
            provider: thread.provider.clone(),
            model: thread.model.clone(),
            thinking_effort: thread.thinking_effort,
            title: thread.title.clone(),
            current_context_tokens: thread.current_context_tokens,
            total_tokens: thread.total_tokens,
            total_cached_tokens: thread.total_cached_tokens,
            llm_context_version: thread.llm_context_version,
            created_at: thread.created_at,
            updated_at: thread.updated_at,
        })
        .exec(&mut tx)
        .await?;
        toasty::create!(BackgroundTask {
            task_id: task.task_id.clone(),
            owner_thread_id: task.owner_thread_id.clone(),
            kind: omini_domain::task::TaskKind::SubAgent,
            title: task.title.clone(),
            status: task.status,
            result_summary: None,
            created_at: task.created_at,
            updated_at: task.updated_at,
            completed_at: None,
            notification_delivered: false,
        })
        .exec(&mut tx)
        .await?;
        toasty::create!(AgentTask {
            task_id: task.task_id.clone(),
            owner_thread_id: task.owner_thread_id.clone(),
            agent_thread_id: task.thread_id.clone(),
            parent_run_id: task.parent_run_id.clone(),
            parent_task_id: task.parent_task_id.clone(),
            parent_thread_id: task.parent_thread_id.clone(),
            spawn_tool_use_id: task.spawn_tool_use_id.clone(),
            depth: i64::from(task.depth),
            execution_mode: task.execution_mode,
            status: task.status,
            agent_name: task.agent.clone(),
            title: task.title.clone(),
            result: None,
            created_at: task.created_at,
            updated_at: task.updated_at,
            completed_at: None,
            notification_delivered: false,
        })
        .exec(&mut tx)
        .await?;
        toasty::create!(AgentRun {
            id: task.task_id.clone(),
            thread_id: task.thread_id.clone(),
            parent_run_id: task.parent_run_id.clone(),
            status: AgentRunStatus::Running,
            created_at: task.created_at,
            started_at: Some(task.created_at),
            finished_at: None,
            total_tokens: 0,
            archived_at: None,
        })
        .exec(&mut tx)
        .await?;
        toasty::create!(Message {
            thread_id: task.thread_id.clone(),
            role: Role::User,
            model_ref: None,
            content: initial_entry,
            kind: MessageKind::ConversationEntry,
            created_at: task.created_at,
        })
        .exec(&mut tx)
        .await?;
        toasty::create!(Message {
            thread_id: task.thread_id.clone(),
            role: Role::User,
            model_ref: None,
            content: initial_content.clone(),
            kind: MessageKind::Normal,
            created_at: task.created_at,
        })
        .exec(&mut tx)
        .await?;
        toasty::create!(LlmMessage {
            thread_id: task.thread_id.clone(),
            context_version: 1,
            ordinal: 0,
            role: Role::User,
            content: initial_content,
            created_at: task.created_at,
        })
        .exec(&mut tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn list_agent_tasks(
        &self,
        owner_thread_id: &str,
    ) -> Result<Vec<AgentTask>, StoreError> {
        let mut db = self.conn();
        Ok(
            AgentTask::filter(AgentTask::fields().owner_thread_id().eq(owner_thread_id))
                .order_by((
                    AgentTask::fields().created_at().asc(),
                    AgentTask::fields().task_id().asc(),
                ))
                .exec(&mut db)
                .await?,
        )
    }

    /// 按主线程归属查找子任务,避免将子线程 ID 误当作主线程 ID 查询 Run。
    pub async fn get_owned_task(
        &self,
        owner_thread_id: &str,
        task_id: &str,
    ) -> Result<Option<AgentTask>, StoreError> {
        let mut db = self.conn();
        Ok(AgentTask::filter_by_task_id(task_id)
            .first()
            .exec(&mut db)
            .await?
            .filter(|row| row.owner_thread_id == owner_thread_id))
    }

    /// 任务终态同时结算 agent_task、background_task 与对应 Run 三处投影,
    /// 三条写入在同一事务内提交。
    pub async fn finish_agent_task(
        &self,
        task_id: &str,
        status: TaskStatus,
        result: &AgentTaskResult,
        completed_at: Timestamp,
    ) -> Result<(), StoreError> {
        let result_json = toasty::stmt::Json(result.clone());
        let mut conn = self.conn();
        let mut tx = conn.transaction().await?;
        if let Some(mut task) = AgentTask::filter_by_task_id(task_id)
            .first()
            .exec(&mut tx)
            .await?
        {
            toasty::update!(task {
                status,
                result: Some(result_json.clone()),
                updated_at: completed_at,
                completed_at: Some(completed_at),
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
                status,
                result_summary: result.output.clone(),
                updated_at: completed_at,
                completed_at: Some(completed_at),
            })
            .exec(&mut tx)
            .await?;
        }
        let run_status = match status {
            TaskStatus::Running | TaskStatus::Cancelling => AgentRunStatus::Running,
            TaskStatus::Completed => AgentRunStatus::Completed,
            TaskStatus::Failed => AgentRunStatus::Failed,
            TaskStatus::Cancelled => AgentRunStatus::Cancelled,
            TaskStatus::Interrupted => AgentRunStatus::Interrupted,
        };
        if let Some(mut run) = AgentRun::filter_by_id(task_id)
            .first()
            .exec(&mut tx)
            .await?
        {
            toasty::update!(run {
                status: run_status,
                finished_at: Some(completed_at),
            })
            .exec(&mut tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// 仅 running 状态的任务转入 cancelling;条件更新保持在事务内原子完成。
    pub async fn set_agent_tasks_cancelling(
        &self,
        task_ids: &[String],
        updated_at: Timestamp,
    ) -> Result<(), StoreError> {
        let mut conn = self.conn();
        let mut tx = conn.transaction().await?;
        for task_id in task_ids {
            if let Some(mut task) = AgentTask::filter_by_task_id(task_id)
                .first()
                .exec(&mut tx)
                .await?
                && task.status == TaskStatus::Running
            {
                toasty::update!(task {
                    status: TaskStatus::Cancelling,
                    updated_at: updated_at,
                })
                .exec(&mut tx)
                .await?;
            }
            if let Some(mut background) = BackgroundTask::filter_by_task_id(task_id)
                .first()
                .exec(&mut tx)
                .await?
                && background.status == TaskStatus::Running
            {
                toasty::update!(background {
                    status: TaskStatus::Cancelling,
                    updated_at: updated_at,
                })
                .exec(&mut tx)
                .await?;
            }
        }
        tx.commit().await?;
        Ok(())
    }

    /// 任务完成通知幂等落库:仅在存在未投递通知的任务时写入 UI 与模型上下文。
    /// 闸门判定、两条插入与置位在同一事务内完成。
    pub async fn insert_task_notification(
        &self,
        owner_thread_id: &str,
        notification: &omini_domain::conversation::TaskNotification,
        llm_message: &omini_model::message::Message,
        task_ids: &[String],
        created_at: Timestamp,
        thread_dir: &ThreadDir,
    ) -> Result<(), StoreError> {
        let mut conn = self.conn();
        let mut tx = conn.transaction().await?;
        let has_pending_task = BackgroundTask::filter(
            BackgroundTask::fields()
                .task_id()
                .in_list(task_ids.to_vec())
                .and(BackgroundTask::fields().notification_delivered().eq(false)),
        )
        .first()
        .exec(&mut tx)
        .await?
        .is_some();
        if !has_pending_task {
            tx.commit().await?;
            return Ok(());
        }

        let owner = Thread::filter_by_id(owner_thread_id)
            .first()
            .exec(&mut tx)
            .await?
            .ok_or_else(|| StoreError::MissingRow(format!("thread '{owner_thread_id}'")))?;
        let model_ref = format!("{}/{}", owner.provider, owner.model);
        let notification_json = serde_json::to_string(&ConversationEntry::SystemEvent(
            omini_domain::conversation::SystemEvent::TaskNotification(notification.clone()),
        ))?;
        // UI 消息经统一的 sidecar 路径写入,与其它会话条目一致;
        // 与后续插入、置位同事务,保持闸门原子性。
        insert_ui_json(
            NewUiJson {
                thread_id: owner_thread_id,
                role: Role::Assistant,
                model_ref: Some(&model_ref),
                content: &notification_json,
                kind: MessageKind::ConversationEntry,
                created_at,
            },
            thread_dir,
            &mut tx,
        )
        .await?;
        let version = owner.llm_context_version;
        let ordinal = next_llm_ordinal(&mut tx, owner_thread_id, version).await?;
        let llm_json = serde_json::to_string(&llm_message.content)?;
        toasty::create!(LlmMessage {
            thread_id: owner_thread_id.to_string(),
            context_version: version,
            ordinal,
            role: Role::User,
            content: llm_json,
            created_at: created_at,
        })
        .exec(&mut tx)
        .await?;
        for task_id in task_ids {
            if let Some(mut background) = BackgroundTask::filter_by_task_id(task_id)
                .first()
                .exec(&mut tx)
                .await?
                && !background.notification_delivered
            {
                toasty::update!(background {
                    notification_delivered: true,
                    updated_at: created_at,
                })
                .exec(&mut tx)
                .await?;
            }
            if let Some(mut task) = AgentTask::filter_by_task_id(task_id)
                .first()
                .exec(&mut tx)
                .await?
                && !task.notification_delivered
            {
                toasty::update!(task {
                    notification_delivered: true,
                    updated_at: created_at,
                })
                .exec(&mut tx)
                .await?;
            }
        }
        tx.commit().await?;
        Ok(())
    }
}

/// 断言线程存在;缺失按内部数据错误处理(事务随返回回滚)。
async fn ensure_thread_exists(
    executor: &mut impl toasty::db::Executor,
    thread_id: &str,
) -> Result<(), StoreError> {
    Thread::filter_by_id(thread_id)
        .first()
        .exec(executor)
        .await?
        .map(|_| ())
        .ok_or_else(|| StoreError::MissingRow(format!("thread '{thread_id}'")))
}

/// 断言子任务存在。
async fn ensure_task_exists(
    executor: &mut impl toasty::db::Executor,
    task_id: &str,
) -> Result<(), StoreError> {
    AgentTask::filter_by_task_id(task_id)
        .first()
        .exec(executor)
        .await?
        .map(|_| ())
        .ok_or_else(|| StoreError::MissingRow(format!("agent task '{task_id}'")))
}

/// 断言 Run 存在。
async fn ensure_run_exists(
    executor: &mut impl toasty::db::Executor,
    run_id: &str,
) -> Result<(), StoreError> {
    AgentRun::filter_by_id(run_id)
        .first()
        .exec(executor)
        .await?
        .map(|_| ())
        .ok_or_else(|| StoreError::MissingRow(format!("agent run '{run_id}'")))
}
