use crate::{store, thread::ThreadSession};
use jiff::Timestamp;
use omini_config::project::ThreadDir;
use omini_core::CoreError;
use omini_domain::conversation::UserInput as ConversationUserInput;
use omini_domain::input::{AttachmentMetadata, UserInputIntent};
use omini_runtime_contract::thread_domain::ClientMessage;
use omini_runtime_contract::{self as runtime_contract, thread::ResolvedAttachment};

impl ThreadSession {
    pub(crate) fn cwd(&self) -> &std::path::Path {
        &self.settings.cwd
    }

    pub(crate) fn thread_dir(&self) -> ThreadDir {
        self.project.thread(&self.thread_id)
    }

    pub(crate) async fn persist_staged_attachment(
        &self,
        staging_path: &std::path::Path,
        size: u64,
        sha256: String,
        mime_type: &str,
        original_name: &str,
    ) -> Result<store::Attachment, CoreError> {
        let (relative_path, created) =
            store::persist_staged_asset(&self.thread_dir(), staging_path, &sha256, mime_type)
                .map_err(|error| {
                    let _ = std::fs::remove_file(staging_path);
                    CoreError::persistence("failed to persist attachment", error.to_string())
                })?;
        let attachment = store::Attachment {
            id: uuid::Uuid::new_v4().to_string(),
            thread_id: self.thread_id.clone(),
            original_name: original_name.to_string(),
            mime_type: mime_type.to_string(),
            size: i64::try_from(size).map_err(|_| {
                CoreError::invalid_input("attachment_too_large", "attachment is too large")
            })?,
            sha256,
            relative_path,
            created_at: Timestamp::now(),
            thread: Default::default(),
        };
        if let Err(error) = self.db.create_attachment(&attachment).await {
            if created {
                let _ =
                    std::fs::remove_file(self.thread_dir().path().join(&attachment.relative_path));
            }
            return Err(CoreError::persistence(
                "failed to persist attachment metadata",
                error.to_string(),
            ));
        }
        Ok(attachment)
    }

    pub(crate) async fn get_attachment(
        &self,
        attachment_id: &str,
    ) -> Result<Option<store::Attachment>, CoreError> {
        self.db
            .get_attachment(&self.thread_id, attachment_id)
            .await
            .map_err(|error| {
                CoreError::persistence("failed to load attachment metadata", error.to_string())
            })
    }

    pub(crate) fn load_attachment_bytes(
        &self,
        attachment: &store::Attachment,
    ) -> Result<Vec<u8>, CoreError> {
        let size = u64::try_from(attachment.size).map_err(|_| {
            CoreError::persistence(
                "invalid attachment metadata",
                format!("attachment '{}' has a negative size", attachment.id),
            )
        })?;
        store::load_asset(
            &self.thread_dir(),
            &attachment.relative_path,
            size,
            &attachment.sha256,
        )
        .map_err(|error| {
            CoreError::persistence("failed to read attachment content", error.to_string())
        })
    }

    pub(crate) async fn resolve_attachments(
        &self,
        attachment_ids: &[String],
    ) -> Result<Vec<ResolvedAttachment>, CoreError> {
        let records = self
            .db
            .get_attachments(&self.thread_id, attachment_ids)
            .await
            .map_err(|error| match error {
                store::StoreError::AttachmentNotFound(id) => CoreError::invalid_input(
                    "attachment_not_found",
                    format!("attachment '{id}' does not exist in this thread"),
                ),
                other => CoreError::persistence(
                    "failed to resolve attachment metadata",
                    other.to_string(),
                ),
            })?;
        records
            .into_iter()
            .map(|record| {
                let size = u64::try_from(record.size).map_err(|_| {
                    CoreError::persistence(
                        "invalid attachment metadata",
                        format!("attachment '{}' has a negative size", record.id),
                    )
                })?;
                let source_path =
                    store::stored_asset_path(&self.thread_dir(), &record.relative_path).map_err(
                        |error| {
                            CoreError::persistence("invalid attachment metadata", error.to_string())
                        },
                    )?;
                Ok(ResolvedAttachment {
                    metadata: AttachmentMetadata {
                        attachment_id: record.id,
                        mime_type: record.mime_type,
                        size,
                        name: record.original_name,
                    },
                    sha256: record.sha256,
                    source_path,
                })
            })
            .collect()
    }

    pub async fn reload_subagent_registry(&self) -> Result<(), CoreError> {
        self.handle.reload_subagent_registry().await
    }

    /// 发起会话关闭：停止接收新工作，取消前台运行与任务树并收尾。幂等。
    ///
    /// 实例收尾完成后消费者任务自然退出；缓存摘除由 manager 完成。
    pub async fn close(&self) -> Result<(), CoreError> {
        self.handle.close().await?;
        self.consumer_finished
            .clone()
            .wait_for(|finished| *finished)
            .await
            .map(|_| ())
            .map_err(|_| CoreError::RuntimeClosed)
    }

    /// 最后连接断开与空闲回收共用原子资格检查，避免关闭新受理的工作。
    pub fn begin_idle_close(&self) -> bool {
        let presence = self.presence.lock().expect("presence lock poisoned");
        presence.connection_counts.is_empty() && self.handle.begin_idle_close()
    }

    pub fn matches_instance(&self, handle: &omini_core::execution::AgentHandle) -> bool {
        self.handle.same_instance(handle)
    }

    /// 测试专用：预留一次运行，使会话处于不可回收的忙碌状态。
    #[cfg(test)]
    pub(crate) fn reserve_run_for_test(
        &self,
    ) -> Result<omini_core::execution::RunReservation, CoreError> {
        self.handle.reserve_run()
    }

    pub async fn set_model(
        &self,
        command: runtime_contract::thread::SetModelCommand,
    ) -> Result<(), CoreError> {
        self.handle.set_model(command).await
    }

    pub fn list_models(&self) -> runtime_contract::thread::ModelsSnapshot {
        self.handle.list_models()
    }

    pub async fn toggle_active_profile(&self) -> Result<(), CoreError> {
        self.handle.toggle_active_profile().await
    }

    pub async fn set_active_profile(
        &self,
        command: runtime_contract::thread::SetActiveProfileCommand,
    ) -> Result<(), CoreError> {
        self.handle.set_active_profile(command.profile).await
    }

    pub async fn compact_context(&self, instructions: Option<String>) -> Result<(), CoreError> {
        self.handle.compact_context(instructions).await
    }

    /// 向当前线程提交一次新的运行请求。
    ///
    /// 预留/提交流程：先原子预留运行资格（忙碌时返回 `run_busy`，被拒绝的
    /// 输入不落库），预留成功后持久化展示行与初始 Run 记录、广播 echo，最后
    /// 提交实例执行。持久化失败时丢弃预留即可，实例不执行任何运行。
    pub async fn submit_run(
        self: &std::sync::Arc<Self>,
        command: runtime_contract::thread::SubmitRunCommand,
    ) -> Result<runtime_contract::thread::RunSubmitted, CoreError> {
        let prepared = self.handle.prepare_run(command)?;
        self.submit_prepared_run(prepared).await
    }

    pub fn prepare_run(
        &self,
        command: runtime_contract::thread::SubmitRunCommand,
    ) -> Result<runtime_contract::thread::PreparedRunCommand, CoreError> {
        self.handle.prepare_run(command)
    }

    pub async fn submit_prepared_run(
        self: &std::sync::Arc<Self>,
        command: runtime_contract::thread::PreparedRunCommand,
    ) -> Result<runtime_contract::thread::RunSubmitted, CoreError> {
        // InterveneMessage intent 在持久化展示前先确认当前确有运行，
        // 避免把"干预"错存成新输入。
        if matches!(
            command.intent,
            runtime_contract::thread::RunIntent::InterveneMessage
        ) {
            let snapshot = self.handle.snapshot();
            if snapshot.current_run.is_none() {
                return Err(CoreError::invalid_input(
                    "no_active_run",
                    "Cannot intervene because no run is active",
                ));
            }
            self.commit_user_input_display(&command).await?;
            self.handle.intervene(command.message).await.map(|_| {
                runtime_contract::thread::RunSubmitted {
                    run_id: "current".to_string(),
                }
            })
        } else {
            let reservation = self.handle.reserve_run()?;
            let acceptance = self.acceptances.begin();
            let session = std::sync::Arc::clone(self);
            let (result_tx, result_rx) = tokio::sync::oneshot::channel();
            // 受理任务由 server 持有：事务一旦开始就完成提交或回滚，HTTP future
            // 消失不会在已提交输入与启动确认之间丢弃预留。尚未开始时可直接释放。
            tokio::spawn(async move {
                let _acceptance = acceptance;
                if result_tx.is_closed() {
                    return;
                }
                let result = async {
                    let run_id = reservation.run_id().to_string();
                    let input = display_input(&command);
                    session
                        .db
                        .accept_run_input(
                            &domain_agent_run_snapshot(&run_id, &session.thread_id),
                            &input,
                            &session.thread_dir(),
                        )
                        .await
                        .map_err(|error| {
                            CoreError::persistence("failed to accept user input", error.to_string())
                        })?;
                    session.broadcast_server_local_event(omini_protocol::RuntimeEvent::new(
                        omini_protocol::TypedRuntimeEvent::UserMessageInjected {
                            item: omini_protocol::HistoryItem::UserInput(input),
                            client_echo_id: command.client_echo_id,
                        },
                    ));
                    if let Err(error) = reservation.commit(command.message).await {
                        // 输入已经受理；实例同时关闭等启动失败需明确结算运行，
                        // 保留已接受的用户历史，而不能留下永远 Running 的记录。
                        session
                            .db
                            .update_agent_run(
                                &run_id,
                                omini_domain::agent_run::AgentRunStatus::Failed,
                                None,
                                Some(Timestamp::now()),
                                0,
                            )
                            .await
                            .map_err(|failure| {
                                CoreError::persistence(
                                    "failed to settle unstarted run",
                                    failure.to_string(),
                                )
                            })?;
                        return Err(error);
                    }
                    Ok(runtime_contract::thread::RunSubmitted {
                        run_id: "current".to_string(),
                    })
                }
                .await;
                let _ = result_tx.send(result);
            });
            result_rx
                .await
                .map_err(|_| CoreError::new("run acceptance task failed"))?
        }
    }

    /// 用户输入生命周期中属于 server 的部分：展示行立即落库（时间戳 = 发言时刻，
    /// 因此 replay 呈现发言时间视角），echo 随后在本地事件通道广播，两者都先于
    /// 提交实例执行。core 只在安全输入边界提交 LLM 上下文行。
    async fn commit_user_input_display(
        &self,
        command: &runtime_contract::thread::PreparedRunCommand,
    ) -> Result<(), CoreError> {
        let input = display_input(command);
        self.db
            .insert_user_input(
                &self.thread_id,
                &input,
                Timestamp::now(),
                &self.thread_dir(),
            )
            .await
            .map_err(|error| {
                CoreError::persistence("failed to persist user input", error.to_string())
            })?;
        let echo = omini_protocol::RuntimeEvent::new(
            omini_protocol::TypedRuntimeEvent::UserMessageInjected {
                item: omini_protocol::HistoryItem::UserInput(input),
                client_echo_id: command.client_echo_id.clone(),
            },
        );
        self.broadcast_server_local_event(echo);
        Ok(())
    }

    pub async fn cancel_run(&self) -> Result<(), CoreError> {
        self.handle.cancel_current().await
    }

    pub async fn cancel_agent_run(&self, run_id: String) -> Result<(), CoreError> {
        self.handle.cancel_agent_run(run_id).await
    }

    /// 先持久化子会话展示消息，再将结构化输入投递到子任务队列。
    pub async fn send_task_input(
        &self,
        run_id: String,
        child_thread_id: String,
        client_id: String,
        command: runtime_contract::thread::SubmitRunCommand,
    ) -> Result<(), CoreError> {
        let prepared = self.handle.prepare_run(command)?;
        let source = client_message(&client_id, &prepared)?;
        let fresh = self
            .db
            .enqueue_client_message(
                &run_id,
                &self.thread_id,
                &child_thread_id,
                &source,
                &self.project.thread(&child_thread_id),
            )
            .await
            .map_err(|error| match error {
                store::StoreError::InvalidData(message) => {
                    CoreError::invalid_input("delivery_key_conflict", message)
                }
                other => {
                    CoreError::persistence("failed to enqueue child user input", other.to_string())
                }
            })?;
        if !fresh {
            return Ok(());
        }
        self.broadcast_server_local_event(omini_protocol::RuntimeEvent::new(
            omini_protocol::TypedRuntimeEvent::AgentTaskUserMessageQueued {
                task_id: run_id.clone(),
                thread_id: child_thread_id,
                item: omini_protocol::HistoryItem::UserInput(source.input.clone()),
                client_id: Some(source.client_id.clone()),
                client_echo_id: Some(source.client_echo_id.clone()),
            },
        ));
        if let Err(error) = self
            .handle
            .intervene_agent_run(run_id.clone(), prepared.message, Some(source.clone()))
            .await
        {
            // 受理语义在 DB 层（登记 + 展示历史 + 广播已成功）；实例侧注入
            // 失败（任务已结束或不接受输入）不回滚受理，走既有投递失败结算，
            // 数量随任务结果的 undelivered_messages 报告。
            tracing::warn!(
                error = %error,
                task_id = %run_id,
                "child user input accepted but not injected"
            );
            self.db
                .fail_client_message(&run_id, &source, "子任务已结束，客户端输入未能注入")
                .await
                .map_err(|store_error| {
                    CoreError::persistence(
                        "failed to settle child user input",
                        store_error.to_string(),
                    )
                })?;
        }
        Ok(())
    }

    /// 终态之后仍允许相同来源键的重试得到幂等成功。
    pub async fn task_input_replayed(
        &self,
        task_id: &str,
        client_id: &str,
        command: &runtime_contract::thread::SubmitRunCommand,
    ) -> Result<bool, CoreError> {
        let prepared = self.handle.prepare_run(command.clone())?;
        let source = client_message(client_id, &prepared)?;
        let existing = self
            .db
            .client_delivery(task_id, &source)
            .await
            .map_err(|error| {
                CoreError::persistence("failed to inspect child user input", error.to_string())
            })?;
        let Some((payload, status)) = existing else {
            return Ok(false);
        };
        if payload
            != serde_json::to_string(&source.input).expect("user input serialization cannot fail")
        {
            return Err(CoreError::invalid_input(
                "delivery_key_conflict",
                "client echo ID was reused with different content",
            ));
        }
        if status == store::DeliveryStatus::Failed {
            return Err(CoreError::invalid_input(
                "delivery_failed",
                "previous child user input was not delivered",
            ));
        }
        Ok(true)
    }

    pub async fn resolve_tool_pause(
        &self,
        command: runtime_contract::thread::ResolveToolPauseCommand,
    ) -> Result<(), CoreError> {
        self.handle.resolve_tool_pause(command).await
    }

    pub async fn resolve_plan(
        &self,
        command: runtime_contract::thread::ResolvePlanCommand,
    ) -> Result<(), CoreError> {
        self.handle.resolve_plan(command).await
    }

    pub fn list_skills(&self) -> Vec<runtime_contract::thread::SkillSummarySnapshot> {
        self.handle.list_skills()
    }

    pub async fn set_thinking_effort(
        &self,
        command: runtime_contract::thread::SetThinkingEffortCommand,
    ) -> Result<(), CoreError> {
        self.handle.set_thinking_effort(command).await
    }
}

/// 外部提交的初始 Run 记录；预留 RunId 即数据库主键。
fn domain_agent_run_snapshot(
    run_id: &str,
    thread_id: &str,
) -> omini_domain::agent_run::AgentRunSnapshot {
    let now = Timestamp::now();
    omini_domain::agent_run::AgentRunSnapshot {
        id: run_id.to_string(),
        thread_id: thread_id.to_string(),
        parent_run_id: None,
        status: omini_domain::agent_run::AgentRunStatus::Running,
        created_at: now,
        started_at: Some(now),
        finished_at: None,
        total_tokens: 0,
        archived_at: None,
    }
}

fn client_message(
    client_id: &str,
    prepared: &runtime_contract::thread::PreparedRunCommand,
) -> Result<ClientMessage, CoreError> {
    let client_echo_id = prepared
        .client_echo_id
        .clone()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| {
            CoreError::invalid_input(
                "missing_client_echo_id",
                "child AgentRun input requires client_echo_id",
            )
        })?;
    let mut attachments = prepared
        .input
        .attachments
        .iter()
        .map(|attachment| attachment.metadata.clone())
        .collect::<Vec<_>>();
    attachments.sort_by(|left, right| left.attachment_id.cmp(&right.attachment_id));
    Ok(ClientMessage {
        client_id: client_id.to_string(),
        client_echo_id,
        input: ConversationUserInput {
            intent: UserInputIntent::Message,
            parts: prepared.input.parts.clone(),
            attachments,
        },
    })
}

/// 从已校验输入构建展示历史，保持原始来源与稳定附件顺序。
fn display_input(command: &runtime_contract::thread::PreparedRunCommand) -> ConversationUserInput {
    let intent = match command.intent {
        runtime_contract::thread::RunIntent::SubmitMessage
        | runtime_contract::thread::RunIntent::InterveneMessage => UserInputIntent::Message,
        runtime_contract::thread::RunIntent::ExecuteCommand(command) => {
            UserInputIntent::Command { command }
        }
    };
    let mut attachments = command
        .input
        .attachments
        .iter()
        .map(|attachment| attachment.metadata.clone())
        .collect::<Vec<_>>();
    attachments.sort_by(|left, right| left.attachment_id.cmp(&right.attachment_id));
    ConversationUserInput {
        intent,
        parts: command.input.parts.clone(),
        attachments,
    }
}
