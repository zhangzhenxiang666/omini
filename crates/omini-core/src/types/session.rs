use crate::types::config::ThinkingEffort;
use crate::types::display::UserDraft;
use crate::types::events::{ActiveProfile, PlanApprovalAction, ToolPauseResponse};

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
