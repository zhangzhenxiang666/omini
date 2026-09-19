use super::*;

struct AgentMessagePersistence<'a> {
    thread_id: &'a str,
    message: &'a Message,
    model_ref: Option<&'a str>,
    persist_llm_history: bool,
    display_in_ui: bool,
    created_at: DateTime<Utc>,
    project: &'a ProjectDir,
}

impl Database {
    async fn persist_agent_message(
        &self,
        request: AgentMessagePersistence<'_>,
    ) -> Result<(), StoreError> {
        if request.display_in_ui {
            self.insert_message(
                &NewMessage {
                    thread_id: request.thread_id.to_string(),
                    role: request.message.role.to_string(),
                    model_ref: (request.message.role == Role::Assistant)
                        .then(|| request.model_ref.map(str::to_string))
                        .flatten(),
                    blocks: request.message.content.clone(),
                    kind: "normal".to_string(),
                    created_at: request.created_at,
                },
                &request.project.thread(request.thread_id),
            )
            .await?;
        }
        if request.persist_llm_history {
            self.append_llm_message(
                request.thread_id,
                request.message,
                request.created_at,
                &request.project.thread(request.thread_id),
            )
            .await?;
        }
        Ok(())
    }
    pub async fn apply_persistence_event(
        &self,
        event: &RuntimePersistenceEvent,
        project_id: &str,
        project: &ProjectDir,
    ) -> Result<(), StoreError> {
        match event {
            RuntimePersistenceEvent::CreateAgentTask {
                task,
                thread,
                initial_message,
                ..
            } => {
                self.create_agent_task(project_id, task, thread, initial_message)
                    .await
            }
            RuntimePersistenceEvent::PersistAgentMessage {
                thread_id,
                message,
                model_ref,
                persist_llm_history,
                display_in_ui,
                ..
            } => {
                self.persist_agent_message(AgentMessagePersistence {
                    thread_id,
                    message,
                    model_ref: model_ref.as_deref(),
                    persist_llm_history: *persist_llm_history,
                    display_in_ui: *display_in_ui,
                    created_at: Utc::now(),
                    project,
                })
                .await
            }
            RuntimePersistenceEvent::FinishAgentTask {
                task_id,
                status,
                result,
                completed_at,
                ..
            } => {
                self.finish_agent_task(task_id, *status, result, *completed_at)
                    .await
            }
            RuntimePersistenceEvent::SetAgentTasksCancelling { task_ids } => {
                self.set_agent_tasks_cancelling(task_ids, Utc::now()).await
            }
            RuntimePersistenceEvent::InsertAgentTaskNotification {
                owner_thread_id,
                notification,
                llm_message,
                task_ids,
                ..
            } => {
                self.insert_agent_task_notification(
                    owner_thread_id,
                    notification,
                    llm_message,
                    task_ids,
                    Utc::now(),
                )
                .await
            }
            RuntimePersistenceEvent::UpdateThreadUpdatedAt { thread_id } => {
                self.update_thread_updated_at(thread_id).await
            }
            RuntimePersistenceEvent::UpdateThreadConfig {
                thread_id,
                provider,
                model,
                thinking_effort,
            } => {
                self.update_thread_config(thread_id, provider, model, thinking_effort.as_deref())
                    .await
            }
            RuntimePersistenceEvent::UpdateThreadThinkingEffort {
                thread_id,
                thinking_effort,
            } => {
                self.update_thread_thinking_effort(thread_id, thinking_effort.as_deref())
                    .await
            }
            RuntimePersistenceEvent::UiMessageAppended {
                thread_id,
                message,
                model_ref,
            } => {
                self.insert_message(
                    &NewMessage {
                        thread_id: thread_id.clone(),
                        role: message.role.to_string(),
                        model_ref: model_ref.clone(),
                        blocks: message.content.clone(),
                        kind: "normal".to_string(),
                        created_at: Utc::now(),
                    },
                    &project.thread(thread_id),
                )
                .await
            }
            RuntimePersistenceEvent::InsertPlanMessage {
                thread_id,
                plan,
                model_ref,
            } => {
                self.insert_plan_message(thread_id, plan, model_ref, &project.thread(thread_id))
                    .await
            }
            RuntimePersistenceEvent::InsertCompactSummaryMessage {
                thread_id,
                summary,
                model_ref,
            } => {
                self.insert_compact_summary_message(
                    thread_id,
                    summary,
                    model_ref,
                    &project.thread(thread_id),
                )
                .await
            }
            RuntimePersistenceEvent::AppendLlmMessage { thread_id, message } => {
                self.append_llm_message(thread_id, message, Utc::now(), &project.thread(thread_id))
                    .await
            }
            RuntimePersistenceEvent::ReplaceLlmContext {
                thread_id,
                expected_version,
                messages,
                ..
            } => self
                .replace_llm_context(
                    thread_id,
                    *expected_version,
                    messages,
                    Utc::now(),
                    &project.thread(thread_id),
                )
                .await
                .map(|_| ()),
            RuntimePersistenceEvent::RecordThreadUsage { thread_id, usage } => {
                self.record_thread_usage(thread_id, *usage).await
            }
            RuntimePersistenceEvent::RecordThreadTotalUsage { thread_id, usage } => {
                self.record_thread_total_usage(thread_id, *usage).await
            }
            RuntimePersistenceEvent::RecordOwnerAgentUsage { thread_id, usage } => {
                self.record_thread_total_usage(thread_id, *usage).await
            }
        }
    }
}
