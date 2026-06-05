use super::service::AgentRuntime;

impl AgentRuntime {
    pub(super) fn reload_subagent_registry(&mut self) {
        self.capabilities.reload_subagents(&self.settings);
        self.rebuild_system_prompt();
    }
}
