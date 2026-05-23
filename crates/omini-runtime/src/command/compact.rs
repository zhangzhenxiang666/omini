use crate::command::Command;
use crate::runtime::AgentRuntime;
use crate::types::events::{CommandEffect, CommandResult};
use async_trait::async_trait;

pub struct CompactCommand;

#[async_trait]
impl Command for CompactCommand {
    fn name(&self) -> &str {
        "compact"
    }

    fn aliases(&self) -> &[&'static str] {
        &[]
    }

    fn description(&self) -> &str {
        "压缩当前会话上下文"
    }

    fn args_description(&self) -> Option<&'static str> {
        Some("[custom summarization instructions]")
    }

    fn has_args(&self) -> bool {
        true
    }

    fn sort_weight(&self) -> i32 {
        30
    }

    async fn execute(&self, runtime: &mut AgentRuntime, args: &str) -> CommandResult {
        let custom_instructions = (!args.trim().is_empty()).then_some(args.trim());
        match runtime
            .force_compact_current_session(custom_instructions)
            .await
        {
            Ok(()) => CommandResult::Ok(Vec::new()),
            Err(error) => CommandResult::Ok(vec![CommandEffect::Notice(error)]),
        }
    }
}
