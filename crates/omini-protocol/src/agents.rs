//! Agent 管理协议。

use super::*;
use utoipa::ToSchema;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentSourceKind {
    BuiltIn,
    Project,
    User,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AgentDraft {
    pub name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_description: Option<String>,
    pub instructions: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disallow_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct GeneratedAgentDraft {
    pub name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_description: Option<String>,
    pub instructions: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AgentSummary {
    pub name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_description: Option<String>,
    pub location: String,
}

impl From<AgentSourceKind> for omini_domain::subagents::AgentSourceKind {
    fn from(source: AgentSourceKind) -> Self {
        match source {
            AgentSourceKind::BuiltIn => Self::BuiltIn,
            AgentSourceKind::Project => Self::Project,
            AgentSourceKind::User => Self::User,
        }
    }
}

impl From<omini_domain::subagents::AgentSourceKind> for AgentSourceKind {
    fn from(source: omini_domain::subagents::AgentSourceKind) -> Self {
        match source {
            omini_domain::subagents::AgentSourceKind::BuiltIn => Self::BuiltIn,
            omini_domain::subagents::AgentSourceKind::Project => Self::Project,
            omini_domain::subagents::AgentSourceKind::User => Self::User,
        }
    }
}

impl From<AgentDraft> for omini_domain::subagents::AgentDraft {
    fn from(draft: AgentDraft) -> Self {
        Self {
            name: draft.name,
            description: draft.description,
            short_description: draft.short_description,
            instructions: draft.instructions,
            tools: draft.tools,
            disallow_tools: draft.disallow_tools,
            model: draft.model,
        }
    }
}

impl From<omini_domain::subagents::GeneratedAgentDraft> for GeneratedAgentDraft {
    fn from(draft: omini_domain::subagents::GeneratedAgentDraft) -> Self {
        Self {
            name: draft.name,
            description: draft.description,
            short_description: draft.short_description,
            instructions: draft.instructions,
        }
    }
}

impl From<omini_domain::subagents::AgentSummary> for AgentSummary {
    fn from(agent: omini_domain::subagents::AgentSummary) -> Self {
        Self {
            name: agent.name,
            description: agent.description,
            short_description: agent.short_description,
            location: agent.location,
        }
    }
}

impl From<AgentSummary> for omini_domain::subagents::AgentSummary {
    fn from(agent: AgentSummary) -> Self {
        Self {
            name: agent.name,
            description: agent.description,
            short_description: agent.short_description,
            location: agent.location,
        }
    }
}

/// agent 管理接口中的完整 agent 记录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct AgentRecord {
    /// 协议层稳定标识；可编辑 agent 通常是文件路径，内置 agent 可退回名称。
    pub id: String,
    pub name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_description: Option<String>,
    pub instructions: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disallow_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    pub source_kind: AgentSourceKind,
    /// 是否允许客户端基于该记录发起编辑或覆盖保存。
    pub editable: bool,
}

/// agent 管理页面的启动数据响应。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct AgentsResponse {
    pub records: Vec<AgentRecord>,

    pub providers: Vec<ProviderInfo>,
    pub current_provider: String,
    pub current_model: String,
}

/// 保存 agent 草稿的请求，可用于创建或覆盖已有 agent。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SaveAgentRequest {
    pub source_kind: AgentSourceKind,
    /// 编辑已有 agent 时携带原记录 ID；创建新 agent 时为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_agent_id: Option<String>,

    pub draft: AgentDraft,
}

/// 请求模型根据描述生成 agent 草稿。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct GenerateAgentRequest {
    pub description: String,
    pub provider: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_effort: Option<ThinkingEffort>,
}

/// 模型生成的 agent 草稿字段；工具策略、scope 和保存位置由客户端确认后另行保存。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct GenerateAgentResponse {
    pub draft: GeneratedAgentDraft,
}
