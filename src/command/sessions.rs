use super::Command;
use crate::db;
use crate::runtime::AgentRuntime;
use crate::types::events::{CommandResult, InteractionRequest, RuntimeEvent, SessionSummary};
use async_trait::async_trait;

pub struct SessionsCommand;

#[async_trait]
impl Command for SessionsCommand {
    fn name(&self) -> &'static str {
        "sessions"
    }
    fn aliases(&self) -> &[&'static str] {
        &["resume"]
    }
    fn description(&self) -> &'static str {
        "切换会话"
    }
    async fn execute(&self, runtime: &mut AgentRuntime, _args: &str) -> CommandResult {
        let project_path = crate::config::project::sanitize(&runtime.settings.cwd);
        let sessions = match db::global_db().list_sessions(&project_path).await {
            Ok(s) => s,
            Err(e) => return CommandResult::Error(format!("获取会话列表失败: {e}")),
        };
        let mut summaries = Vec::with_capacity(sessions.len());
        for s in sessions {
            let first_message = db::global_db()
                .get_first_message_text(&s.id)
                .await
                .unwrap_or_default();
            summaries.push(SessionSummary {
                id: s.id,
                title: s.title.unwrap_or_default(),
                model: s.model,
                provider: s.provider,
                message_count: s.message_count,
                created_at: s.created_at,
                first_message,
            });
        }
        runtime
            .send_event(RuntimeEvent::InteractionRequest(
                InteractionRequest::SessionSelection {
                    sessions: summaries,
                },
            ))
            .await;
        CommandResult::Pending
    }
}
