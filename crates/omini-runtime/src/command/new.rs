use crate::command::Command;
use crate::runtime::AgentRuntime;
use crate::types::events::{CommandEffect, CommandResult, RuntimeToUiEvent, SessionUsageSnapshot};
use async_trait::async_trait;

pub struct NewCommand;

#[async_trait]
impl Command for NewCommand {
    fn name(&self) -> &str {
        "new"
    }
    fn aliases(&self) -> &[&'static str] {
        &["clear"]
    }
    fn description(&self) -> &str {
        "清空当前会话，开始新对话"
    }
    fn sort_weight(&self) -> i32 {
        20
    }
    async fn execute(
        &self,
        runtime: &mut AgentRuntime,
        _args: &str,
        _draft: &crate::types::display::UserDraft,
    ) -> CommandResult {
        // 清空 runtime 状态
        runtime.session_id = None;
        runtime.session_dir = None;
        runtime.messages.clear();

        CommandResult::Ok(vec![
            CommandEffect::emit(RuntimeToUiEvent::SessionChanged {
                session_id: None,
                messages: vec![],
                subagents: vec![],
                usage: SessionUsageSnapshot {
                    context_window: current_context_window(runtime),
                    ..SessionUsageSnapshot::default()
                },
            }),
            CommandEffect::emit(RuntimeToUiEvent::SessionTitleChanged { title: None }),
        ])
    }
}

fn current_context_window(runtime: &AgentRuntime) -> Option<u32> {
    runtime
        .settings
        .providers
        .get(&runtime.settings.active_provider)
        .and_then(|provider| {
            provider
                .models
                .iter()
                .find(|model| model.id == runtime.settings.model)
                .map(|model| model.limit)
        })
}
