use crate::types::subagents::{AgentDraft, AgentRecord, AgentSourceKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveProjectAgentCommand {
    pub source_kind: AgentSourceKind,
    pub original_agent_id: Option<String>,
    pub draft: AgentDraft,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteProjectAgentCommand {
    pub agent_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentManagementUpdate {
    pub records: Vec<AgentRecord>,
}
