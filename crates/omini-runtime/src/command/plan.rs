use crate::command::Command;
use crate::runtime::AgentRuntime;
use crate::types::events::{ActiveProfile, CommandEffect, CommandResult, RuntimeToUiEvent};
use async_trait::async_trait;

pub struct PlanCommand;

#[async_trait]
impl Command for PlanCommand {
    fn name(&self) -> &str {
        "plan"
    }

    fn aliases(&self) -> &[&'static str] {
        &[]
    }

    fn description(&self) -> &str {
        "切换到 plan mode"
    }

    fn sort_weight(&self) -> i32 {
        25
    }

    async fn execute(
        &self,
        runtime: &mut AgentRuntime,
        _args: &str,
        _draft: &crate::types::display::UserDraft,
    ) -> CommandResult {
        runtime.set_active_profile(ActiveProfile::Plan);
        CommandResult::Ok(vec![CommandEffect::emit(
            RuntimeToUiEvent::ActiveProfileChanged(runtime.active_profile()),
        )])
    }
}
