//! core 执行宿主的 server 实现：把 `AgentHost` 操作落到 SQLite store，
//! 并完成两个宿主侧投影——任务完成通知与排队子 Agent 消息的本地事件广播。

use crate::event::replay::RuntimeReplayBuffer;
use crate::store::{Store, StoreError};
use jiff::Timestamp;
use omini_config::project::ProjectDir;
use omini_core::execution::{AgentHost, AgentSession, AgentSessionRequest, HostError};
use omini_domain as domain;
use omini_entity::NewThread;
use omini_protocol as client_proto;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

/// 会话宿主：core 的持久化与资源申请都经由它进入 store。
///
/// 错误统一转为 [`HostError`]；成功返回即已落库。本线程 UI 行落库成功后
/// 同步裁剪 replay 尾部，保持重连补发与快照不重复。
pub struct SessionHost {
    db: Arc<Store>,
    project_id: String,
    project: ProjectDir,
    owner_thread_id: String,
    replay_buffer: Arc<Mutex<RuntimeReplayBuffer>>,
    server_event_inbox: tokio::sync::mpsc::UnboundedSender<client_proto::RuntimeEvent>,
}

impl SessionHost {
    pub fn new(
        db: Arc<Store>,
        project_id: String,
        project: ProjectDir,
        owner_thread_id: String,
        replay_buffer: Arc<Mutex<RuntimeReplayBuffer>>,
        server_event_inbox: tokio::sync::mpsc::UnboundedSender<client_proto::RuntimeEvent>,
    ) -> Self {
        Self {
            db,
            project_id,
            project,
            owner_thread_id,
            replay_buffer,
            server_event_inbox,
        }
    }

    fn thread_dir(&self, thread_id: &str) -> omini_config::project::ThreadDir {
        self.project.thread(thread_id)
    }

    fn store_error(context: &'static str) -> impl Fn(StoreError) -> HostError {
        move |error| HostError::new(context, error.to_string())
    }

    /// UI 展示行落库：模型消息转 conversation entry 后写入 messages 表。
    async fn append_ui_row(
        &self,
        thread_id: &str,
        message: &omini_model::message::Message,
        model_ref: Option<&str>,
    ) -> Result<(), HostError> {
        let Some(entry) = crate::conversation::entry_from_model_message(message.clone()) else {
            return Ok(());
        };
        self.db
            .insert_conversation_entry(
                thread_id,
                &entry,
                message.role,
                model_ref,
                Timestamp::now(),
                &self.thread_dir(thread_id),
            )
            .await
            .map_err(Self::store_error("append ui message"))?;
        // 只有本线程的展示行参与 replay 裁剪；子 Agent 线程的行不在本会话回放。
        if thread_id == self.owner_thread_id {
            self.replay_buffer
                .lock()
                .expect("replay buffer lock poisoned")
                .record_persisted_ui_message(message);
        }
        Ok(())
    }

    /// 主 Agent → 子任务消息：登记待投递记录，成功且是新消息时投影本地事件。
    async fn enqueue_agent_message_inner(
        &self,
        task_id: &str,
        owner_thread_id: &str,
        agent_thread_id: &str,
        message: &domain::conversation::AgentMessage,
    ) -> Result<(), HostError> {
        let fresh = self
            .db
            .enqueue_agent_message(
                task_id,
                owner_thread_id,
                agent_thread_id,
                message,
                &self.thread_dir(agent_thread_id),
            )
            .await
            .map_err(Self::store_error("enqueue agent message"))?;
        if fresh {
            let _ = self
                .server_event_inbox
                .send(client_proto::RuntimeEvent::new(
                    client_proto::TypedRuntimeEvent::AgentTaskMessageQueued {
                        task_id: task_id.to_string(),
                        thread_id: agent_thread_id.to_string(),
                        item: client_proto::HistoryItem::SystemEvent(
                            domain::conversation::SystemEvent::AgentMessage(message.clone()),
                        ),
                    },
                ));
        }
        Ok(())
    }

    /// 原子建立子 Agent 会话：分配子线程 ID、建目录并落库全部初始行。
    async fn create_agent_session_inner(
        &self,
        request: AgentSessionRequest,
    ) -> Result<AgentSession, HostError> {
        let thread_id = Uuid::new_v4().to_string();
        let thread_dir = self
            .project
            .create_thread(&thread_id)
            .map_err(|error| HostError::new("create agent thread directory", error.to_string()))?;
        let now = Timestamp::now();
        let task = omini_runtime_contract::thread_domain::AgentTaskInfo {
            task_id: request.task_id.clone(),
            thread_id: thread_id.clone(),
            parent_run_id: request.parent_run_id.clone(),
            parent_task_id: request.parent_task_id.clone(),
            owner_thread_id: request.owner_thread_id.clone(),
            parent_thread_id: request.parent_thread_id.clone(),
            spawn_tool_use_id: request.spawn_tool_use_id.clone(),
            agent: request.agent.clone(),
            title: request.title.clone(),
            depth: request.depth,
            execution_mode: request.execution_mode,
            status: domain::task::TaskStatus::Running,
            result: None,
            created_at: now,
            updated_at: now,
            completed_at: None,
            notification_delivered: false,
        };
        let thread = NewThread {
            id: thread_id.clone(),
            parent_thread_id: Some(request.parent_thread_id.clone()),
            spawn_tool_use_id: Some(request.spawn_tool_use_id.clone()),
            thread_type: "agent".to_string(),
            agent_label: Some(request.agent.clone()),
            provider: request.model.provider.clone(),
            model: request.model.model.clone(),
            thinking_effort: request.model.thinking_effort.clone(),
            title: Some(request.title.clone()),
            current_context_tokens: 0,
            total_tokens: 0,
            total_cached_tokens: 0,
            llm_context_version: 1,
            created_at: now,
            updated_at: now,
        };
        if let Err(error) = self
            .db
            .create_agent_task(
                &self.project_id,
                &task,
                &thread,
                &request.initial_prompt,
                &request.initial_message,
            )
            .await
        {
            // 本次申请独占新分配的目录；事务失败不能留下无记录的子会话资源。
            if let Err(cleanup) = std::fs::remove_dir_all(thread_dir.path()) {
                tracing::warn!(%cleanup, "failed to remove uncommitted agent directory");
            }
            return Err(Self::store_error("create agent task")(error));
        }
        Ok(AgentSession {
            thread_id,
            thread_dir,
        })
    }
}

#[async_trait::async_trait]
impl AgentHost for SessionHost {
    async fn create_agent_run(
        &self,
        run: &domain::agent_run::AgentRunSnapshot,
    ) -> Result<(), HostError> {
        self.db
            .create_agent_run(run)
            .await
            .map_err(Self::store_error("create agent run"))
    }

    async fn update_agent_run(
        &self,
        run_id: &str,
        status: domain::agent_run::AgentRunStatus,
        started_at: Option<Timestamp>,
        finished_at: Option<Timestamp>,
        add_tokens: i64,
    ) -> Result<(), HostError> {
        self.db
            .update_agent_run(run_id, status, started_at, finished_at, add_tokens)
            .await
            .map_err(Self::store_error("update agent run"))
    }

    async fn upsert_agent_step(
        &self,
        step: &domain::agent_run::AgentStepSnapshot,
    ) -> Result<(), HostError> {
        self.db
            .upsert_agent_step(step)
            .await
            .map_err(Self::store_error("upsert agent step"))
    }

    async fn update_agent_step(
        &self,
        step_id: &str,
        status: domain::agent_run::AgentStepStatus,
        finished_at: Option<Timestamp>,
        add_input_tokens: i64,
        add_output_tokens: i64,
    ) -> Result<(), HostError> {
        self.db
            .update_agent_step(
                step_id,
                status,
                finished_at,
                add_input_tokens,
                add_output_tokens,
            )
            .await
            .map_err(Self::store_error("update agent step"))
    }

    async fn upsert_tool_use_execution(
        &self,
        tool_use: &domain::agent_run::ToolUseExecutionSnapshot,
        status: domain::agent_run::ToolUseStatus,
    ) -> Result<(), HostError> {
        self.db
            .upsert_tool_use_execution(tool_use, status)
            .await
            .map_err(Self::store_error("upsert tool use execution"))
    }

    async fn append_llm_message(
        &self,
        thread_id: &str,
        message: &omini_model::message::Message,
    ) -> Result<(), HostError> {
        self.db
            .append_llm_message(
                thread_id,
                message,
                Timestamp::now(),
                &self.thread_dir(thread_id),
            )
            .await
            .map_err(Self::store_error("append llm message"))
    }

    async fn append_ui_message(
        &self,
        thread_id: &str,
        message: &omini_model::message::Message,
        model_ref: Option<&str>,
    ) -> Result<(), HostError> {
        self.append_ui_row(thread_id, message, model_ref).await
    }

    async fn insert_plan_message(
        &self,
        thread_id: &str,
        plan: &domain::conversation::ProposedPlan,
        model_ref: &str,
    ) -> Result<(), HostError> {
        self.db
            .insert_plan_message(thread_id, plan, model_ref, &self.thread_dir(thread_id))
            .await
            .map_err(Self::store_error("insert plan message"))
    }

    async fn insert_compact_summary(
        &self,
        thread_id: &str,
        summary: &domain::conversation::CompactionSummary,
        model_ref: &str,
    ) -> Result<(), HostError> {
        self.db
            .insert_compact_summary_message(
                thread_id,
                summary,
                model_ref,
                &self.thread_dir(thread_id),
            )
            .await
            .map_err(Self::store_error("insert compact summary"))?;
        if thread_id == self.owner_thread_id {
            self.replay_buffer
                .lock()
                .expect("replay buffer lock poisoned")
                .record_persisted_compact_summary();
        }
        Ok(())
    }

    async fn replace_llm_context(
        &self,
        thread_id: &str,
        expected_version: i64,
        messages: Vec<omini_model::message::Message>,
    ) -> Result<i64, HostError> {
        self.db
            .replace_llm_context(
                thread_id,
                expected_version,
                &messages,
                Timestamp::now(),
                &self.thread_dir(thread_id),
            )
            .await
            .map_err(Self::store_error("replace llm context"))
    }

    async fn record_thread_usage(
        &self,
        thread_id: &str,
        usage: domain::usage::Usage,
    ) -> Result<(), HostError> {
        self.db
            .record_thread_usage(thread_id, usage)
            .await
            .map_err(Self::store_error("record thread usage"))
    }

    async fn record_thread_total_usage(
        &self,
        thread_id: &str,
        usage: domain::usage::Usage,
    ) -> Result<(), HostError> {
        self.db
            .record_thread_total_usage(thread_id, usage)
            .await
            .map_err(Self::store_error("record thread total usage"))
    }

    async fn record_owner_agent_usage(
        &self,
        owner_thread_id: &str,
        usage: domain::usage::Usage,
    ) -> Result<(), HostError> {
        // 故意复用 record_thread_total_usage：owner 的 total_tokens 是自身运行
        // 与全部子 Agent 上行两路累加的合计，写同一行。
        self.db
            .record_thread_total_usage(owner_thread_id, usage)
            .await
            .map_err(Self::store_error("record owner agent usage"))
    }

    async fn update_thread_config(
        &self,
        thread_id: &str,
        provider: &str,
        model: &str,
        thinking_effort: Option<&str>,
    ) -> Result<(), HostError> {
        self.db
            .update_thread_config(thread_id, provider, model, thinking_effort)
            .await
            .map_err(Self::store_error("update thread config"))
    }

    async fn update_thread_thinking_effort(
        &self,
        thread_id: &str,
        thinking_effort: Option<&str>,
    ) -> Result<(), HostError> {
        self.db
            .update_thread_thinking_effort(thread_id, thinking_effort)
            .await
            .map_err(Self::store_error("update thread thinking effort"))
    }

    async fn touch_thread(&self, thread_id: &str) -> Result<(), HostError> {
        self.db
            .update_thread_updated_at(thread_id)
            .await
            .map_err(Self::store_error("touch thread"))
    }

    async fn upsert_background_task(&self, task: &domain::task::TaskInfo) -> Result<(), HostError> {
        self.db
            .upsert_task(task)
            .await
            .map_err(Self::store_error("upsert background task"))
    }

    async fn insert_task_notification(
        &self,
        owner_thread_id: &str,
        notification: &domain::conversation::TaskNotification,
    ) -> Result<Option<domain::conversation::TaskNotification>, HostError> {
        let inserted = self
            .db
            .insert_task_notification(
                owner_thread_id,
                notification,
                Timestamp::now(),
                &self.thread_dir(owner_thread_id),
            )
            .await
            .map_err(Self::store_error("insert task notification"))?;
        // 仅实际写入的新内容才投影为注入的 user message；store 去重后没有
        // 新任务时不广播，客户端从快照与回放都不会看到重复通知。
        if let Some(fresh) = &inserted {
            let _ = self
                .server_event_inbox
                .send(client_proto::RuntimeEvent::new(
                    client_proto::TypedRuntimeEvent::UserMessageInjected {
                        item: client_proto::HistoryItem::SystemEvent(
                            domain::conversation::SystemEvent::TaskNotification(fresh.clone()),
                        ),
                        client_echo_id: None,
                    },
                ));
        }
        Ok(inserted)
    }

    async fn create_agent_session(
        &self,
        request: AgentSessionRequest,
    ) -> Result<AgentSession, HostError> {
        self.create_agent_session_inner(request).await
    }

    async fn persist_agent_message(
        &self,
        agent_thread_id: &str,
        message: &omini_model::message::Message,
        model_ref: Option<&str>,
        persist_llm_history: bool,
        display_in_ui: bool,
    ) -> Result<(), HostError> {
        self.db
            .persist_agent_message(
                crate::store::AgentMessageCommit {
                    thread_id: agent_thread_id,
                    message,
                    model_ref,
                    persist_llm_history,
                    display_in_ui,
                },
                &self.thread_dir(agent_thread_id),
            )
            .await
            .map_err(Self::store_error("persist agent message"))?;
        if display_in_ui && agent_thread_id == self.owner_thread_id {
            self.replay_buffer
                .lock()
                .expect("replay buffer lock poisoned")
                .record_persisted_ui_message(message);
        }
        Ok(())
    }

    async fn enqueue_agent_message(
        &self,
        task_id: &str,
        owner_thread_id: &str,
        agent_thread_id: &str,
        message: &domain::conversation::AgentMessage,
    ) -> Result<(), HostError> {
        self.enqueue_agent_message_inner(task_id, owner_thread_id, agent_thread_id, message)
            .await
    }

    async fn inject_task_message(
        &self,
        key: &omini_runtime_contract::thread_domain::DeliveryKey,
        agent_thread_id: &str,
        model_message: &omini_model::message::Message,
    ) -> Result<(), HostError> {
        self.db
            .inject_task_message(
                agent_thread_id,
                key,
                model_message,
                &self.thread_dir(agent_thread_id),
            )
            .await
            .map_err(Self::store_error("inject task message"))
    }

    async fn fail_pending_task_messages(
        &self,
        task_id: &str,
        reason: &str,
    ) -> Result<u32, HostError> {
        self.db
            .fail_task_messages(task_id, reason)
            .await
            .map_err(Self::store_error("fail pending task messages"))
    }

    async fn finish_agent_task(
        &self,
        task_id: &str,
        status: domain::task::TaskStatus,
        result: &omini_runtime_contract::thread_domain::AgentTaskResult,
        completed_at: Timestamp,
    ) -> Result<(), HostError> {
        self.db
            .finish_agent_task(task_id, status, result, completed_at)
            .await
            .map_err(Self::store_error("finish agent task"))
    }

    async fn set_agent_tasks_cancelling(&self, task_ids: &[String]) -> Result<(), HostError> {
        self.db
            .set_agent_tasks_cancelling(task_ids, Timestamp::now())
            .await
            .map_err(Self::store_error("set agent tasks cancelling"))
    }
}
