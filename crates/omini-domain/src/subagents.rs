use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct AgentRecord {
    pub name: String,
    pub description: String,
    pub instructions: String,
    pub tools: Vec<String>,
    pub disallow_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub source_kind: AgentSourceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    pub editable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct AgentDraft {
    pub name: String,
    pub description: String,
    pub instructions: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disallow_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct AgentSummary {
    pub name: String,
    pub description: String,
}
