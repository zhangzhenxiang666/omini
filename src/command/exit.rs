use super::Command;
use crate::runtime::AgentRuntime;
use crate::types::events::{CommandResult, RuntimeToUiEvent};
use async_trait::async_trait;

pub struct ExitCommand;

#[async_trait]
impl Command for ExitCommand {
    fn name(&self) -> &'static str {
        "exit"
    }
    fn aliases(&self) -> &[&'static str] {
        &["quit"]
    }
    fn description(&self) -> &'static str {
        "退出程序"
    }
    async fn execute(&self, runtime: &mut AgentRuntime, _args: &str) -> CommandResult {
        runtime.send_event(RuntimeToUiEvent::Shutdown).await;
        CommandResult::Done
    }
}
