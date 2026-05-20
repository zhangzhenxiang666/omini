use crate::command::Command;
use crate::db;
use crate::runtime::AgentRuntime;
use crate::types::events::{CommandEffect, CommandResult, InteractionRequest, SessionSummary};
use async_trait::async_trait;

pub struct SessionsCommand;

#[async_trait]
impl Command for SessionsCommand {
    fn name(&self) -> &str {
        "sessions"
    }
    fn aliases(&self) -> &[&'static str] {
        &["resume"]
    }
    fn description(&self) -> &str {
        "切换会话"
    }
    fn sort_weight(&self) -> i32 {
        10
    }
    async fn execute(&self, runtime: &mut AgentRuntime, _args: &str) -> CommandResult {
        let project_path = crate::config::project::sanitize(&runtime.settings.cwd);
        let sessions = match db::global_db().list_sessions(&project_path).await {
            Ok(s) => s,
            Err(e) => return CommandResult::Error(format!("获取会话列表失败: {e}")),
        };
        let mut summaries = Vec::with_capacity(sessions.len());
        for s in sessions {
            summaries.push(SessionSummary {
                id: s.id,
                title: s.title.unwrap_or_default(),
                model: s.model,
                provider: s.provider,
                message_count: s.message_count,
                created_at: s.created_at,
            });
        }
        CommandResult::Ok(vec![CommandEffect::ShowInteraction(
            InteractionRequest::SessionSelection {
                sessions: summaries,
            },
        )])
    }
}
