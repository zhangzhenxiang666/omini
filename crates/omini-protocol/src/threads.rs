//! 线程状态、模型与输入协议。

use super::*;
use utoipa::ToSchema;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ActiveProfile {
    #[default]
    Main,
    Auto,
    Plan,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ThreadRuntimeState {
    #[default]
    Idle,
    Working,
    Thinking,
    Waiting,
    Compacting,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ThreadSummary {
    pub id: String,
    pub title: String,
    pub model: String,
    pub provider: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_state: Option<ThreadRuntimeState>,
}

impl ActiveProfile {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Auto => "auto",
            Self::Plan => "plan",
        }
    }
}

impl std::fmt::Display for ActiveProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<ActiveProfile> for omini_runtime_contract::thread_domain::ActiveProfile {
    fn from(profile: ActiveProfile) -> Self {
        match profile {
            ActiveProfile::Main => Self::Main,
            ActiveProfile::Auto => Self::Auto,
            ActiveProfile::Plan => Self::Plan,
        }
    }
}

impl From<omini_runtime_contract::thread_domain::ActiveProfile> for ActiveProfile {
    fn from(profile: omini_runtime_contract::thread_domain::ActiveProfile) -> Self {
        match profile {
            omini_runtime_contract::thread_domain::ActiveProfile::Main => Self::Main,
            omini_runtime_contract::thread_domain::ActiveProfile::Auto => Self::Auto,
            omini_runtime_contract::thread_domain::ActiveProfile::Plan => Self::Plan,
        }
    }
}

impl From<ThreadRuntimeState> for omini_runtime_contract::thread_domain::ThreadRuntimeState {
    fn from(state: ThreadRuntimeState) -> Self {
        match state {
            ThreadRuntimeState::Idle => Self::Idle,
            ThreadRuntimeState::Working => Self::Working,
            ThreadRuntimeState::Thinking => Self::Thinking,
            ThreadRuntimeState::Waiting => Self::Waiting,
            ThreadRuntimeState::Compacting => Self::Compacting,
        }
    }
}

impl From<omini_runtime_contract::thread_domain::ThreadRuntimeState> for ThreadRuntimeState {
    fn from(state: omini_runtime_contract::thread_domain::ThreadRuntimeState) -> Self {
        match state {
            omini_runtime_contract::thread_domain::ThreadRuntimeState::Idle => Self::Idle,
            omini_runtime_contract::thread_domain::ThreadRuntimeState::Working => Self::Working,
            omini_runtime_contract::thread_domain::ThreadRuntimeState::Thinking => Self::Thinking,
            omini_runtime_contract::thread_domain::ThreadRuntimeState::Waiting => Self::Waiting,
            omini_runtime_contract::thread_domain::ThreadRuntimeState::Compacting => {
                Self::Compacting
            }
        }
    }
}

impl From<omini_runtime_contract::thread_domain::ThreadSummary> for ThreadSummary {
    fn from(thread: omini_runtime_contract::thread_domain::ThreadSummary) -> Self {
        Self {
            id: thread.id,
            title: thread.title,
            model: thread.model,
            provider: thread.provider,
            created_at: thread.created_at,
            updated_at: thread.updated_at,
            runtime_state: thread.runtime_state.map(Into::into),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ThreadRuntimeActivityKind {
    Query,
    Compact,
}

/// 当前线程正在执行的顶层活动及其计时信息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ThreadRuntimeActivity {
    pub kind: ThreadRuntimeActivityKind,
    pub started_at: DateTime<Utc>,
    /// 已运行时间，单位毫秒；query 活动会扣除等待客户端响应的暂停时长。
    pub elapsed_ms: u64,
}

/// 当前线程或子 agent 正在运行的工具调用。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ThreadRuntimeTool {
    pub tool_use_id: String,
    pub tool_name: String,
    pub started_at: DateTime<Utc>,
    /// 已运行时间，单位毫秒。
    pub elapsed_ms: u64,
    /// 工具来自子 agent 时，这里标识源线程。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_thread_id: Option<String>,
    /// 工具来自子 agent 时，这里提供人类可读的 agent 标签。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_agent_label: Option<String>,
}

/// 当前线程可见的 skill 运行态信息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ThreadRuntimeSkill {
    pub name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_description: Option<String>,
    pub source_kind: SkillSourceKind,
    pub directory: String,
    pub status: ThreadRuntimeCapabilityStatus,
    pub disable_model_invocation: bool,
    pub user_invocable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SkillSourceKind {
    BuiltIn,
    Project,
    User,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ThreadRuntimeCapabilityStatus {
    Available,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ThreadRuntimeMcpStatus {
    Disabled,
    Connecting,
    Ready,
    Failed,
}

/// MCP server 暴露给模型的单个工具。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ThreadRuntimeMcpTool {
    pub name: String,
    /// 经过 daemon 去重后真正注册给模型使用的工具名。
    pub registered_name: String,
    pub description: String,
}

/// 当前线程可见的单个 MCP server 状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ThreadRuntimeMcpServer {
    pub name: String,
    pub status: ThreadRuntimeMcpStatus,
    /// 最近一次连接或初始化失败原因；非失败状态通常为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ThreadRuntimeMcpTool>,
}

/// 线程运行态的完整协议快照，供新连接或状态轮询同步 UI。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ThreadRuntimeStatus {
    pub thread_id: String,
    /// 当前线程顶层运行状态。
    pub state: ThreadRuntimeState,
    /// 当前线程使用的运行 profile。
    #[serde(default)]
    pub active_profile: ActiveProfile,
    /// core 是否已完成该线程的加载和 hydrate。
    pub loaded: bool,
    /// 当前拥有线程控制权的客户端 ID；无人控制时为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controller_id: Option<String>,
    /// 当前订阅该线程事件流的客户端数量。
    pub connected_client_count: usize,
    /// 当前顶层活动；空表示没有正在运行或压缩的任务。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<ThreadRuntimeActivity>,
    /// 所有尚未被客户端响应的暂停请求。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_pauses: Vec<ToolPauseRequest>,
    /// 当前等待客户端确认的计划；用于新连接恢复计划审批抽屉。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_plan_approval: Option<PlanSubmittedEvent>,
    /// 当前仍在执行的工具调用。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_tools: Vec<ThreadRuntimeTool>,
    /// 当前线程可见的 skill 能力。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<ThreadRuntimeSkill>,
    /// 当前线程可见的 MCP server 能力和状态。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp_servers: Vec<ThreadRuntimeMcpServer>,
    /// 当前线程可用的子 agent 能力列表。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subagent_threads: Vec<AgentSummary>,
    /// 当前工作目录的 git 分支；不在 git 仓库中时为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
}

/// 项目下多个活跃线程的运行态列表响应。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ThreadStatusesResponse {
    pub statuses: Vec<ThreadRuntimeStatus>,
}

/// 用户输入由有序语义 parts 和无序、线程内附件引用组成。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UserInput {
    pub parts: Vec<InputPart>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachment_ids: Vec<String>,
}

impl UserInput {
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            parts: vec![InputPart::Text { text: text.into() }],
            attachment_ids: Vec::new(),
        }
    }
}

/// 当前线程可用模型列表及正在使用的模型选择。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ModelsResponse {
    pub providers: Vec<ProviderInfo>,
    pub current_provider: String,
    pub current_model: String,
}

/// 设置当前线程 provider、模型和可选 thinking effort 的请求。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SetModelRequest {
    pub provider: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_effort: Option<ThinkingEffort>,
}

/// 设置当前线程 thinking effort 的请求。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SetThinkingEffortRequest {
    pub effort: ThinkingEffort,
}

/// 设置当前线程活跃 provider profile 的请求。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SetActiveProfileRequest {
    pub profile: ActiveProfile,
}

/// 项目下可见线程列表响应。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ThreadsResponse {
    pub threads: Vec<ThreadSummary>,
}

/// TODO: 注意这里如果provider为Some但是model为None那么可能会出问题
/// 创建线程时可覆盖项目默认运行配置。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CreateThreadRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_effort: Option<ThinkingEffort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<ActiveProfile>,
}

/// 创建线程后的响应。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CreateThreadResponse {
    pub thread_id: String,
}

/// 重命名当前线程的请求。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RenameThreadRequest {
    pub title: String,
}

/// 请求压缩当前线程上下文，可携带用户补充指令。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CompactContextRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}
