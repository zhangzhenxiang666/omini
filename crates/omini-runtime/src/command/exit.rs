use super::Command;
use crate::runtime::AgentRuntime;
use crate::types::events::{CommandEffect, CommandResult, RuntimeToUiEvent};
use async_trait::async_trait;

pub struct ExitCommand;

#[async_trait]
impl Command for ExitCommand {
    fn name(&self) -> &str {
        "exit"
    }
    fn aliases(&self) -> &[&'static str] {
        &["quit"]
    }
    fn description(&self) -> &str {
        "退出程序"
    }
    fn sort_weight(&self) -> i32 {
        1000
    }
    async fn execute(&self, _runtime: &mut AgentRuntime, _args: &str) -> CommandResult {
        CommandResult::Ok(vec![CommandEffect::emit(RuntimeToUiEvent::Shutdown)])
    }
}
