use crate::session::SessionRuntime;
use omini_core::CoreError;
use omini_runtime_contract as runtime_contract;

impl SessionRuntime {
    pub async fn reload_subagent_registry(&self) -> Result<(), CoreError> {
        self.core.reload_subagent_registry().await
    }

    pub async fn shutdown(&self) -> Result<(), CoreError> {
        self.core.shutdown().await
    }

    pub async fn set_model(
        &self,
        command: runtime_contract::session::SetModelCommand,
    ) -> Result<(), CoreError> {
        self.core.set_model(command).await
    }

    pub fn list_models(&self) -> runtime_contract::session::ModelsSnapshot {
        self.core.list_models()
    }

    pub async fn toggle_active_profile(&self) -> Result<(), CoreError> {
        self.core.toggle_active_profile().await
    }

    pub async fn set_active_profile(
        &self,
        command: runtime_contract::session::SetActiveProfileCommand,
    ) -> Result<(), CoreError> {
        self.core.set_active_profile(command).await
    }

    pub async fn compact_context(&self, instructions: Option<String>) -> Result<(), CoreError> {
        self.core.compact_context(instructions).await
    }

    pub async fn submit_run(
        &self,
        command: runtime_contract::session::SubmitRunCommand,
    ) -> Result<runtime_contract::session::RunSubmitted, CoreError> {
        self.core.submit_run(command).await
    }

    pub async fn cancel_run(&self) -> Result<(), CoreError> {
        self.core.cancel_run().await
    }

    pub async fn resolve_tool_pause(
        &self,
        command: runtime_contract::session::ResolveToolPauseCommand,
    ) -> Result<(), CoreError> {
        self.core.resolve_tool_pause(command).await
    }

    pub async fn resolve_plan(
        &self,
        command: runtime_contract::session::ResolvePlanCommand,
    ) -> Result<(), CoreError> {
        self.core.resolve_plan(command).await
    }

    pub fn list_skills(&self) -> Vec<runtime_contract::session::SkillSummarySnapshot> {
        self.core.list_skills()
    }

    pub fn get_skill(
        &self,
        skill_name: &str,
    ) -> Option<runtime_contract::session::SkillDetailSnapshot> {
        self.core.get_skill(skill_name)
    }

    pub async fn set_thinking_effort(
        &self,
        command: runtime_contract::session::SetThinkingEffortCommand,
    ) -> Result<(), CoreError> {
        self.core.set_thinking_effort(command).await
    }
}
