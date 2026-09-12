use omini_domain::config::{ProviderInfo, ThinkingEffort};
use omini_domain::events::{ActiveProfile, PlanApprovalAction, ToolPauseResponse};
use omini_domain::input::{PreparedUserSubmission, RunCommand, RuntimeUserInput};
use omini_domain::subagents::AgentRecord;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunIntent {
    SubmitMessage,
    InterveneMessage,
    ExecuteCommand(RunCommand),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmitRunCommand {
    pub input: RuntimeUserInput,
    pub client_echo_id: Option<String>,
    pub intent: RunIntent,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedRunCommand {
    pub submission: PreparedUserSubmission,
    pub client_echo_id: Option<String>,
    pub intent: RunIntent,
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
    pub short_description: Option<String>,
    pub argument_hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeSkillSnapshot {
    pub name: String,
    pub description: String,
    pub short_description: Option<String>,
    pub source_kind: RuntimeSkillSourceKind,
    pub directory: PathBuf,
    pub status: RuntimeCapabilityStatus,
    pub disable_model_invocation: bool,
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
