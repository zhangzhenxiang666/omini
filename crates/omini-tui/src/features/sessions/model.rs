use crate::app::event::{AgentTaskExecutionMode, AgentTaskInfo, AgentTaskSnapshot};
use crate::features::sessions::timing::RunTimer;
use crate::features::timeline::model::UiMessage;
use omini_domain::task::TaskStatus;
use omini_model::message::Message;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone)]
pub struct SessionState {
    pub messages: Vec<UiMessage>,
    /// 时间线派生缓存只属于当前视图，不进入协议、历史或会话快照。
    pub render_cache: RefCell<crate::features::timeline::view::TimelineRenderCache>,
    /// 本地 optimistic echo 的一次性 runtime 回显关联表；只存在于当前 TUI 进程内。
    pub pending_client_echoes: HashMap<String, Vec<usize>>,
    /// 正在流式构建中的 assistant 消息（SSE 实时显示）
    pub pending_assistant: Option<crate::features::timeline::model::StreamingMessage>,
    /// 正在流式构建中的 proposed plan markdown。
    pub pending_proposed_plan: Option<String>,
    /// 正在流式构建中的 compact 摘要（含呼吸动画，不走缓存）
    pub pending_compact_summary: Option<String>,
    /// 当前思考段的本地计时起点；收到首个 thinking delta 时开启，
    /// 被 text/tool 事件结算写入 pending_assistant 的 Thinking 块 duration_ms。
    /// 渲染层据此显示动态 "Thinking for Xs..."。
    pub thinking_started_at: Option<std::time::Instant>,
    /// 渲染后的消息总行数（用于滚动条计算）
    pub total_lines: usize,
    /// 当前渲染出的全部消息行纯文本，用于鼠标拖选反查内容。
    pub selectable_message_lines: Vec<String>,
    /// 当前消息视口顶部对应 selectable_message_lines 的行号。
    pub message_scroll_y: usize,
    /// 主 agent 的 query 是否正在运行；后台任务不影响普通输入和 intervention 的分流。
    pub main_query_active: bool,
    pub agent_status: AgentStatus,
    /// 手动 /compact 命令是否正在执行。compact 不走普通 query 生命周期。
    pub manual_compact_running: bool,
    /// 当前 query 的有效运行计时器；等待用户授权/回答时暂停。
    pub run_timer: Option<RunTimer>,
    /// 从底部向上滚动的行数（0 = 位于底部，显示最新消息）
    pub scroll_offset: usize,
    /// 自动滚动锁定：true = 有新内容时自动保持在底部；false = 用户手动浏览历史不跳转
    pub auto_scroll: bool,
    /// 正在运行中的工具 ID 集合（已收到 ToolUse 但尚未收到 ToolResult）
    pub running_tools: HashSet<String>,
}

impl Default for SessionState {
    fn default() -> Self {
        Self {
            messages: Vec::new(),
            render_cache: RefCell::new(Default::default()),
            pending_client_echoes: HashMap::new(),
            pending_assistant: None,
            pending_proposed_plan: None,
            pending_compact_summary: None,
            thinking_started_at: None,
            total_lines: 0,
            selectable_message_lines: Vec::new(),
            message_scroll_y: 0,
            main_query_active: false,
            agent_status: AgentStatus::Idle,
            manual_compact_running: false,
            run_timer: None,
            scroll_offset: 0,
            auto_scroll: true,
            running_tools: HashSet::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub enum AgentStatus {
    #[default]
    Idle,
    /// LLM 思考中
    Thinking,
    /// 工具执行中
    Working,
    /// 等待用户操作（权限确认/回答问题）
    AwaitingInput,
}

impl std::fmt::Display for AgentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentStatus::Idle => write!(f, "Ready"),
            AgentStatus::Thinking => write!(f, "Thinking"),
            AgentStatus::Working => write!(f, "Working"),
            AgentStatus::AwaitingInput => write!(f, "Waiting for you"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentNode {
    pub task_id: String,
    pub thread_id: String,
    pub parent_thread_id: String,
    pub spawn_tool_use_id: String,
    pub agent_label: String,
    pub title: String,
    pub execution_mode: AgentTaskExecutionMode,
    pub status: TaskStatus,
    /// Agent 结束后显示的实际运行时长。
    pub duration: Option<std::time::Duration>,
    /// 从任务快照恢复的创建时间，用于列表实时计时和缺少完成时间时的回退计算。
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub messages: Vec<Message>,
}

impl From<AgentTaskSnapshot> for SubagentNode {
    fn from(snapshot: AgentTaskSnapshot) -> Self {
        Self::from(snapshot.task)
    }
}

impl From<AgentTaskInfo> for SubagentNode {
    fn from(task: AgentTaskInfo) -> Self {
        let started_at = task.created_at;
        let duration = task
            .completed_at
            .or_else(|| task.status.is_terminal().then_some(task.updated_at))
            .and_then(|completed_at| (completed_at - started_at).to_std().ok());
        Self {
            task_id: task.task_id,
            thread_id: task.thread_id,
            parent_thread_id: task.parent_thread_id,
            spawn_tool_use_id: task.spawn_tool_use_id,
            agent_label: task.agent,
            title: task.title,
            execution_mode: task.execution_mode,
            status: task.status,
            duration,
            started_at,
            messages: Vec::new(),
        }
    }
}
