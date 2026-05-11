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
        let cmds = runtime.command_registry.all_commands();
        // 计算左半部分（"/name" + 可选别名）的最大宽度
        let max_width = cmds
            .iter()
            .map(|cmd| {
                let left = if cmd.aliases().is_empty() {
                    format!("/{}", cmd.name())
                } else {
                    format!("/{} (别名: {})", cmd.name(), cmd.aliases().join(", "))
                };
                left.chars().count()
            })
            .max()
            .unwrap_or(0);

        let mut help = String::from("可用的命令:\n");
        for cmd in cmds {
            let left = if cmd.aliases().is_empty() {
                format!("/{}", cmd.name())
            } else {
                format!("/{} (别名: {})", cmd.name(), cmd.aliases().join(", "))
            };
            let padding = " ".repeat(max_width.saturating_sub(left.chars().count()));
            help.push_str(&format!("  {}{}  — {}\n", left, padding, cmd.description()));
        }
        runtime.send_event(RuntimeEvent::CommandOutput(help)).await;
        CommandResult::Done
    }
}
