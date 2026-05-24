use crate::command::Command;
use crate::runtime::AgentRuntime;
use crate::types::events::{CommandEffect, CommandResult, RuntimeToUiEvent};
use async_trait::async_trait;

pub struct HelpCommand;

#[async_trait]
impl Command for HelpCommand {
    fn name(&self) -> &str {
        "help"
    }
    fn aliases(&self) -> &[&'static str] {
        &["?"]
    }
    fn description(&self) -> &str {
        "显示帮助"
    }
    fn sort_weight(&self) -> i32 {
        900
    }
    async fn execute(
        &self,
        runtime: &mut AgentRuntime,
        _args: &str,
        _draft: &crate::types::display::UserDraft,
    ) -> CommandResult {
        CommandResult::Ok(vec![CommandEffect::emit(RuntimeToUiEvent::ShowHelpDrawer(
            runtime.command_registry.summaries(),
        ))])
    }
}
