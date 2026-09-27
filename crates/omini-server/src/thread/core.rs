use crate::{store, thread::ThreadRuntime};
use chrono::Utc;
use omini_config::project::ThreadDir;
use omini_core::CoreError;
use omini_domain::conversation::UserInput as ConversationUserInput;
use omini_domain::input::{AttachmentMetadata, UserInputIntent};
use omini_runtime_contract::persistence::ClientMessage;
use omini_runtime_contract::{self as runtime_contract, thread::ResolvedAttachment};

impl ThreadRuntime {
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
            created_at: Utc::now(),
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
        self.core.reload_subagent_registry().await
    }

    pub async fn shutdown(&self) -> Result<(), CoreError> {
        self.core.shutdown().await
    }

    pub async fn set_model(
        &self,
        command: runtime_contract::thread::SetModelCommand,
    ) -> Result<(), CoreError> {
        self.core.set_model(command).await
    }

    pub fn list_models(&self) -> runtime_contract::thread::ModelsSnapshot {
        self.core.list_models()
    }

    pub async fn toggle_active_profile(&self) -> Result<(), CoreError> {
        self.core.toggle_active_profile().await
    }

    pub async fn set_active_profile(
        &self,
        command: runtime_contract::thread::SetActiveProfileCommand,
    ) -> Result<(), CoreError> {
        self.core.set_active_profile(command).await
    }

    pub async fn compact_context(&self, instructions: Option<String>) -> Result<(), CoreError> {
        self.core.compact_context(instructions).await
    }

    pub async fn submit_run(
        &self,
        command: runtime_contract::thread::SubmitRunCommand,
    ) -> Result<runtime_contract::thread::RunSubmitted, CoreError> {
        self.core.submit_run(command).await
    }

    pub fn prepare_run(
        &self,
        command: runtime_contract::thread::SubmitRunCommand,
    ) -> Result<runtime_contract::thread::PreparedRunCommand, CoreError> {
        self.core.prepare_run(command)
    }

    pub async fn submit_prepared_run(
        &self,
        command: runtime_contract::thread::PreparedRunCommand,
    ) -> Result<runtime_contract::thread::RunSubmitted, CoreError> {
        self.commit_user_input_display(&command).await?;
        self.core.submit_prepared_run(command).await
    }

    /// 用户输入生命周期中属于 server 的部分：展示行立即落库（时间戳 = 发言时刻，
    /// 因此 replay 呈现发言时间视角），echo 随后在本地事件通道广播，两者都先于
    /// core 派发完成。core 只在安全输入边界提交 LLM 上下文行。
    async fn commit_user_input_display(
        &self,
        command: &runtime_contract::thread::PreparedRunCommand,
    ) -> Result<(), CoreError> {
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
        let input = ConversationUserInput {
            intent,
            parts: command.input.parts.clone(),
            attachments,
        };
        self.db
            .insert_user_input(&self.thread_id, &input, Utc::now(), &self.thread_dir())
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
        self.core.cancel_run().await
    }

    pub async fn cancel_agent_run(&self, run_id: String) -> Result<(), CoreError> {
        self.core.cancel_agent_run(run_id).await
    }

    /// 先持久化子会话展示消息，再将结构化输入投递到子任务队列。
    pub async fn send_task_input(
        &self,
        run_id: String,
        child_thread_id: String,
        client_id: String,
        command: runtime_contract::thread::SubmitRunCommand,
    ) -> Result<(), CoreError> {
        let prepared = self.core.prepare_run(command)?;
        let source = client_message(&client_id, &prepared)?;
        let fresh = self
            .db
            .enqueue_client_message(&run_id, &self.thread_id, &child_thread_id, &source)
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
            .core
            .intervene_client_run(run_id.clone(), prepared.message, source.clone())
            .await
        {
            self.db
                .fail_client_message(&run_id, &source, "运行时未接收消息")
                .await
                .map_err(|store_error| {
                    CoreError::persistence(
                        "failed to settle child user input",
                        store_error.to_string(),
                    )
                })?;
            return Err(error);
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
        let prepared = self.core.prepare_run(command.clone())?;
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
        if status == "failed" {
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
        self.core.resolve_tool_pause(command).await
    }

    pub async fn resolve_plan(
        &self,
        command: runtime_contract::thread::ResolvePlanCommand,
    ) -> Result<(), CoreError> {
        self.core.resolve_plan(command).await
    }

    pub fn list_skills(&self) -> Vec<runtime_contract::thread::SkillSummarySnapshot> {
        self.core.list_skills()
    }

    pub async fn set_thinking_effort(
        &self,
        command: runtime_contract::thread::SetThinkingEffortCommand,
    ) -> Result<(), CoreError> {
        self.core.set_thinking_effort(command).await
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
