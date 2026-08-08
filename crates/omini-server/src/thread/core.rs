use crate::{store, thread::ThreadRuntime};
use omini_config::project::ThreadDir;
use omini_core::CoreError;
use omini_runtime_contract as runtime_contract;

impl ThreadRuntime {
    pub(crate) fn thread_dir(&self) -> ThreadDir {
        self.project.thread(&self.thread_id)
    }

    pub(crate) fn persist_attachment(
        &self,
        bytes: &[u8],
        mime_type: &str,
    ) -> Result<String, CoreError> {
        store::persist_asset(&self.thread_dir(), bytes, mime_type)
            .map(|(sha256, _)| sha256)
            .map_err(|error| {
                CoreError::persistence("failed to persist attachment", error.to_string())
            })
    }

    pub async fn reload_subagent_registry(&self) -> Result<(), CoreError> {
        self.core.reload_subagent_registry().await
    }

    pub async fn shutdown(&self) -> Result<(), CoreError> {
        self.core.shutdown().await
    }

    pub async fn set_model(
        &self,
        command: runtime_contract::thread::SetModelCommand,
    ) -> Result<(), CoreError> {
        self.core.set_model(command).await
    }

    pub fn list_models(&self) -> runtime_contract::thread::ModelsSnapshot {
        self.core.list_models()
    }

    pub async fn toggle_active_profile(&self) -> Result<(), CoreError> {
        self.core.toggle_active_profile().await
    }

    pub async fn set_active_profile(
        &self,
        command: runtime_contract::thread::SetActiveProfileCommand,
    ) -> Result<(), CoreError> {
        self.core.set_active_profile(command).await
    }

    pub async fn compact_context(&self, instructions: Option<String>) -> Result<(), CoreError> {
        self.core.compact_context(instructions).await
    }

    pub async fn submit_run(
        &self,
        command: runtime_contract::thread::SubmitRunCommand,
    ) -> Result<runtime_contract::thread::RunSubmitted, CoreError> {
        self.core.submit_run(command).await
    }

    pub async fn cancel_run(&self) -> Result<(), CoreError> {
        self.core.cancel_run().await
    }

    pub async fn resolve_tool_pause(
        &self,
        command: runtime_contract::thread::ResolveToolPauseCommand,
    ) -> Result<(), CoreError> {
        self.core.resolve_tool_pause(command).await
    }

    pub async fn resolve_plan(
        &self,
        command: runtime_contract::thread::ResolvePlanCommand,
    ) -> Result<(), CoreError> {
        self.core.resolve_plan(command).await
    }

    pub fn list_skills(&self) -> Vec<runtime_contract::thread::SkillSummarySnapshot> {
        self.core.list_skills()
    }

    pub fn get_skill(
        &self,
        skill_name: &str,
    ) -> Option<runtime_contract::thread::SkillDetailSnapshot> {
        self.core.get_skill(skill_name)
    }

    pub async fn set_thinking_effort(
        &self,
        command: runtime_contract::thread::SetThinkingEffortCommand,
    ) -> Result<(), CoreError> {
        self.core.set_thinking_effort(command).await
    }
}
