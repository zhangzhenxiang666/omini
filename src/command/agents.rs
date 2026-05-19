use crate::command::Command;
use crate::runtime::AgentRuntime;
use crate::types::config::ProviderProfile;
use crate::types::events::{CommandEffect, CommandResult, InteractionRequest};
use async_trait::async_trait;
use std::collections::HashMap;

pub struct AgentsCommand;

#[async_trait]
impl Command for AgentsCommand {
    fn name(&self) -> &'static str {
        "agents"
    }

    fn aliases(&self) -> &[&'static str] {
        &[]
    }

    fn description(&self) -> &'static str {
        "管理 agent"
    }

    fn sort_weight(&self) -> i32 {
        35
    }

    async fn execute(&self, runtime: &mut AgentRuntime, _args: &str) -> CommandResult {
        let records = crate::subagents::list_agent_records(&runtime.settings.cwd);
        let providers: HashMap<String, ProviderProfile> = runtime.settings.providers.clone();
        CommandResult::Ok(vec![CommandEffect::ShowInteraction(
            InteractionRequest::AgentManagement {
                records,
                providers,
                current_provider: runtime.settings.active_provider.clone(),
                current_model: runtime.settings.model.clone(),
            },
        )])
    }
}
