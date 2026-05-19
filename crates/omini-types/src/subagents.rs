use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentSourceKind {
    BuiltIn,
    Project,
    User,
}

impl AgentSourceKind {
    pub fn label(self) -> &'static str {
        match self {
            AgentSourceKind::BuiltIn => "内置",
            AgentSourceKind::Project => "项目",
            AgentSourceKind::User => "用户",
        }
    }
}

#[derive(Debug, Clone)]
pub struct AgentRecord {
    pub name: String,
    pub description: String,
    pub instructions: String,
    pub tools: Vec<String>,
    pub disallow_tools: Vec<String>,
    pub model: Option<String>,
    pub source_kind: AgentSourceKind,
    pub path: Option<PathBuf>,
    pub editable: bool,
}

#[derive(Debug, Clone)]
pub struct AgentDraft {
    pub name: String,
    pub description: String,
    pub instructions: String,
    pub tools: Vec<String>,
    pub disallow_tools: Vec<String>,
    pub model: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AgentSummary {
    pub name: String,
    pub description: String,
}
