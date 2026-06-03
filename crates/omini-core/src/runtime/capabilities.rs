use super::*;

#[derive(Debug)]
pub(super) struct CapabilityStore {
    subagents: RwLock<Arc<AgentRegistry>>,
    skills: RwLock<Arc<SkillRegistry>>,
}

impl CapabilityStore {
    pub(super) fn load(settings: &Settings) -> Self {
        Self {
            subagents: RwLock::new(Arc::new(crate::subagents::load_agent_registry(
                &settings.cwd,
            ))),
            skills: RwLock::new(Arc::new(crate::skills::load_skill_registry(&settings.cwd))),
        }
    }

    pub(super) fn subagent_registry(&self) -> Arc<AgentRegistry> {
        self.subagents
            .read()
            .expect("subagent registry lock poisoned")
            .clone()
    }

    pub(super) fn reload_subagents(&self, settings: &Settings) -> Arc<AgentRegistry> {
        let registry = Arc::new(crate::subagents::load_agent_registry(&settings.cwd));
        *self
            .subagents
            .write()
            .expect("subagent registry lock poisoned") = registry.clone();
        registry
    }

    pub(super) fn skill_registry(&self) -> Arc<SkillRegistry> {
        self.skills
            .read()
            .expect("skill registry lock poisoned")
            .clone()
    }
}
