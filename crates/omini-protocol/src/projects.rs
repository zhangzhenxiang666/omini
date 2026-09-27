//! 项目请求、响应和运行配置协议。

use super::*;

/// 项目路径的即时可用状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProjectPathStatus {
    Ready,
    Missing,
}

/// 一个已持久化注册的项目。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ProjectSummary {
    pub id: String,
    pub name: String,
    pub path: String,
    pub storage_key: String,
    pub path_status: ProjectPathStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_opened_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ProjectsResponse {
    pub projects: Vec<ProjectSummary>,
}

/// 注册当前真实工作目录；同一 canonical path 的请求是幂等的。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CreateProjectRequest {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// 修改项目展示名称，或显式 relink 到新的真实目录。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct UpdateProjectRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// 打开项目后供客户端初始化的完整快照。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct OpenProjectResponse {
    pub project: ProjectSummary,
    /// 项目下可供 TUI 首屏展示或切换的线程列表。
    pub threads: Vec<ThreadSummary>,
    /// open 时当前生效的 provider key。
    pub active_provider: String,
    /// open 时当前生效的模型 ID。
    pub model: String,
    /// 当前模型的 thinking effort；不支持或未设置时为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_effort: Option<ThinkingEffort>,
    /// 当前模型可用的上下文窗口；未知时为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
    /// 当前项目配置到 daemon 的 MCP server 数量。
    pub mcp_server_count: usize,
    /// 项目是否存在可注入的本地 instructions。
    pub has_project_instructions: bool,
    /// open 时可用于 @mention 或 agent 管理入口的 agent 摘要。
    pub agents: Vec<AgentSummary>,
    /// open 时可用于 slash skill 列表的用户可调用 skill 摘要。
    pub skills: Vec<SkillSummary>,
    /// 项目工作目录的 git 分支；不在 git 仓库中时为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
}

/// 一个项目按「全局配置 + 项目覆盖」合并后的可运行状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProjectConfigurationState {
    Ready,
    SetupRequired,
    Invalid,
}

/// 供所有客户端决定显示普通工作区、首次引导还是只读诊断页的项目配置快照。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ProjectConfigurationResponse {
    pub state: ProjectConfigurationState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// provider 缺少模型时，客户端可以预填该 ID；不包含任何 secret。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
}

/// 服务端首次配置入口。api_key 仅用于本次写入 auth.json，绝不出现在响应中。
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct BootstrapProjectConfigurationRequest {
    pub provider_id: String,

    pub protocol: ProviderEndpointKind,
    pub base_url: String,
    pub model_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_variable: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
}

impl std::fmt::Debug for BootstrapProjectConfigurationRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BootstrapProjectConfigurationRequest")
            .field("provider_id", &self.provider_id)
            .field("protocol", &self.protocol)
            .field("base_url", &self.base_url)
            .field("model_id", &self.model_id)
            .field("environment_variable", &self.environment_variable)
            .field("api_key", &self.api_key.as_ref().map(|_| "REDACTED"))
            .finish()
    }
}

/// 项目级运行配置更新后的快照；用于无活跃 thread 的 TUI 状态同步。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ProjectRuntimeConfigResponse {
    pub active_provider: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_effort: Option<ThinkingEffort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
}
