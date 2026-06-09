use crate::types::config::ThinkingEffort;
use crate::types::display::UserDraft;
use crate::types::events::{ActiveProfile, PlanApprovalAction, ToolPauseResponse};
use crate::types::subagents::AgentRecord;
use omini_domain::config::ProviderInfo;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunInputMode {
    Submit,
    Intervene,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmitRunCommand {
    pub draft: UserDraft,
    pub client_echo_id: Option<String>,
    pub mode: RunInputMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunSubmitted {
    pub run_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelsSnapshot {
    pub providers: Vec<ProviderInfo>,
    pub current_provider: String,
    pub current_model: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentsSnapshot {
    pub records: Vec<AgentRecord>,
    pub providers: Vec<ProviderInfo>,
    pub current_provider: String,
    pub current_model: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillSummarySnapshot {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillDetailSnapshot {
    pub name: String,
    pub description: String,
    pub body: String,
    pub directory: PathBuf,
    pub user_invocable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeSkillSnapshot {
    pub name: String,
    pub description: String,
    pub source_kind: RuntimeSkillSourceKind,
    pub directory: PathBuf,
    pub status: RuntimeCapabilityStatus,
    pub inject: bool,
    pub user_invocable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeSkillSourceKind {
    BuiltIn,
    Project,
    User,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeCapabilityStatus {
    Available,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetModelCommand {
    pub provider: String,
    pub model: String,
    pub thinking_effort: Option<ThinkingEffort>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetThinkingEffortCommand {
    pub effort: ThinkingEffort,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetActiveProfileCommand {
    pub profile: ActiveProfile,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolveToolPauseCommand {
    pub tool_use_id: String,
    pub response: ToolPauseResponse,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvePlanCommand {
    pub plan_id: String,
    pub action: PlanApprovalAction,
}
