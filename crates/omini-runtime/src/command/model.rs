use crate::command::Command;
use crate::runtime::AgentRuntime;
use crate::types::config::ProviderProfile;
use crate::types::events::{CommandEffect, CommandResult, InteractionRequest};
use async_trait::async_trait;
use std::collections::HashMap;

pub struct ModelCommand;

#[async_trait]
impl Command for ModelCommand {
    fn name(&self) -> &str {
        "model"
    }
    fn aliases(&self) -> &[&'static str] {
        &[]
    }
    fn description(&self) -> &str {
        "切换模型"
    }
    fn sort_weight(&self) -> i32 {
        30
    }
    async fn execute(
        &self,
        runtime: &mut AgentRuntime,
        _args: &str,
        _draft: &crate::types::display::UserDraft,
    ) -> CommandResult {
        let providers: HashMap<String, ProviderProfile> = runtime.settings.providers.clone();
        let current_provider = runtime.settings.active_provider.clone();
        let current_model = runtime.settings.model.clone();
        CommandResult::Ok(vec![CommandEffect::ShowInteraction(
            InteractionRequest::ModelSelection {
                providers,
                current_provider,
                current_model,
            },
        )])
    }
}
