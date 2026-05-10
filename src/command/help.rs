use async_trait::async_trait;

use crate::runtime::AgentRuntime;
use crate::types::events::{CommandResult, RuntimeEvent};

use super::Command;

pub struct HelpCommand;

#[async_trait]
impl Command for HelpCommand {
    fn name(&self) -> &'static str {
        "help"
    }
    fn aliases(&self) -> &[&'static str] {
        &["?"]
    }
    fn description(&self) -> &'static str {
        "显示帮助"
    }
    async fn execute(&self, runtime: &mut AgentRuntime, _args: &str) -> CommandResult {
        let mut help = String::from("可用的命令:\n");
        for cmd in runtime.command_registry.all_commands() {
            let alias_str = if cmd.aliases().is_empty() {
                String::new()
            } else {
                format!(" (别名: {})", cmd.aliases().join(", "))
            };
            help.push_str(&format!(
                "  /{}{} — {}\n",
                cmd.name(),
                alias_str,
                cmd.description()
            ));
        }
        runtime.send_event(RuntimeEvent::CommandOutput(help)).await;
        CommandResult::Done
    }
}
