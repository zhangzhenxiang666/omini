use crate::types::config::ThinkingEffort;
use crate::types::events::{
    ActiveProfile, CommandSummary, InteractionRequest, Notification, SessionSummary,
    SubagentSnapshot, SubagentStatus, SubmittedPlan, ToolPauseRequest,
};
use omini_domain::display::{DisplayImageAttachment, DisplayMessage, HistoryItem, UserDraft};
use omini_domain::message::Message;
use rand::Rng;
use ratatui::layout::Rect;
use ratatui::text::Line;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::time::Duration;
use tokio::time::Instant;

mod autocomplete;
mod events;
mod input;
mod interaction;
mod mention;
mod permission;
mod scroll;

pub use autocomplete::CommandAutocomplete;
pub use interaction::{
    AgentCreateStep, AgentEditorField, AgentGenerateReturn, AgentManagerState, AgentManagerView,
    AgentModelEntry, InteractionStep, ModelSelectionEntry,
};
pub use mention::{
    InputMention, MentionAutocomplete, MentionCandidate, agent_summaries_to_mention_candidates,
};

pub const PASTE_MARKER_THRESHOLD_CHARS: usize = 512;
pub const PASTE_MARKER_THRESHOLD_NEWLINES: usize = 2;
pub const MAX_INPUT_VISIBLE_LINES: usize = 3;
const DEFAULT_INPUT_WRAP_WIDTH: usize = 80;
const INPUT_PLACEHOLDERS: &[&str] = &[
    "用 /init 生成 AGENTS.md, 先让 omini 认识这个项目",
    "接手陌生项目？让 omini 先画一张结构地图",
    "总结当前改动：做了什么、风险在哪、还缺什么测试",
    "读取 @文件，解释它在整个项目里的职责",
    "读取 @目录，帮我梳理核心模块和调用链",
    "为 @文件 补一组最小但有效的测试",
    "让 @subagent 先调研这个问题，再给我结论",
    "结合 @文件 和 @目录，找出最可能出错的位置",
    "先用 /plan 讨论方案，别急着动代码",
    "用 /agents 创建一个适合当前任务的 subagent",
    "上下文太长了？用 /compact 留下决策和关键线索",
    "用 /help 查看命令、技能和输入技巧",
];
const START_SCREEN_TIPS: &[&str] = &[
    "先用 /plan 把方案聊清楚，再进入实现会更稳。",
    "用 @文件 或 @目录 限定上下文，答案会更贴近当前代码。",
    "陌生项目可以先让 omini 梳理模块职责和调用链。",
    "复杂问题先交给 @subagent 调研，再让主会话做决策。",
    "改动前可以说明验收标准，omini 会更容易选择合适测试。",
    "上下文变长后用 /compact 保留关键决策和线索。",
    "用 /agents 管理适合当前项目的专用 subagent。",
    "让 omini 总结当前 diff，可以快速检查风险和漏测点。",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputPasteMarker {
    pub start_char: usize,
    pub end_char: usize,
    pub marker: String,
    pub full_text: String,
    pub full_char_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputImageAttachment {
    pub start_char: usize,
    pub end_char: usize,
    pub marker: String,
    pub source_path: String,
    pub file_name: String,
}

impl InputImageAttachment {
    pub fn display_attachment(&self) -> DisplayImageAttachment {
        DisplayImageAttachment {
            start_char: self.start_char,
            end_char: self.end_char,
            marker: self.marker.clone(),
            source_path: self.source_path.clone(),
            file_name: self.file_name.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputVisualLine {
    pub start_char: usize,
    pub end_char: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct SelectionPoint {
    /// 可选中渲染行的绝对终端行号。
    pub row: usize,
    /// 可选中渲染行内的显示列号。
    pub col: usize,
}

#[derive(Debug, Clone, Default)]
pub struct TextSelection {
    pub start: SelectionPoint,
    pub end: SelectionPoint,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectableScreenLine {
    pub row: u16,
    pub col: u16,
    pub width: u16,
    pub text: String,
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

#[derive(Debug, Clone)]
pub struct RunTimer {
    started_at: Instant,
    paused_total: Duration,
    pause_started_at: Option<Instant>,
}

impl RunTimer {
    fn started_at(started_at: Instant) -> Self {
        Self {
            started_at,
            paused_total: Duration::ZERO,
            pause_started_at: None,
        }
    }

    fn started_with_elapsed_at(now: Instant, elapsed: Duration, paused: bool) -> Self {
        Self {
            started_at: now.checked_sub(elapsed).unwrap_or(now),
            paused_total: Duration::ZERO,
            pause_started_at: paused.then_some(now),
        }
    }

    fn pause_at(&mut self, now: Instant) {
        if self.pause_started_at.is_none() {
            self.pause_started_at = Some(now);
        }
    }

    fn resume_at(&mut self, now: Instant) {
        let Some(paused_at) = self.pause_started_at.take() else {
            return;
        };
        self.paused_total += now.saturating_duration_since(paused_at);
    }

    fn elapsed_at(&self, now: Instant) -> Duration {
        let active_pause = self
            .pause_started_at
            .map(|paused_at| now.saturating_duration_since(paused_at))
            .unwrap_or(Duration::ZERO);
        now.saturating_duration_since(self.started_at)
            .saturating_sub(self.paused_total + active_pause)
    }

    fn finish_at(mut self, now: Instant) -> Duration {
        self.resume_at(now);
        self.elapsed_at(now)
    }

    pub fn is_paused(&self) -> bool {
        self.pause_started_at.is_some()
    }
}

pub(crate) fn format_run_duration(duration: Duration) -> String {
    let total = duration.as_secs();
    let seconds = total % 60;
    let minutes = (total / 60) % 60;
    let hours = total / 3600;

    if hours > 0 {
        format!("{hours}h{minutes:02}m{seconds:02}s")
    } else if minutes > 0 {
        format!("{minutes}m{seconds:02}s")
    } else {
        format!("{seconds}s")
    }
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

pub(crate) fn pause_preview_tool_use_id(pause: &ToolPauseRequest) -> &str {
    pause
        .preview_tool_use_id
        .as_deref()
        .unwrap_or(&pause.tool_use_id)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiMessage {
    Message(Message),
    Display(DisplayMessage),
    ProposedPlan { text: String },
    RunDivider { elapsed: Duration },
    Notification(Notification),
    CompactSummary { text: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentNode {
    pub session_id: String,
    pub parent_session_id: String,
    pub spawn_tool_use_id: String,
    pub agent_label: String,
    pub status: SubagentStatus,
    pub messages: Vec<Message>,
}

impl From<SubagentSnapshot> for SubagentNode {
    fn from(snapshot: SubagentSnapshot) -> Self {
        Self {
            session_id: snapshot.session_id,
            parent_session_id: snapshot.parent_session_id,
            spawn_tool_use_id: snapshot.spawn_tool_use_id,
            agent_label: snapshot.agent_label,
            status: snapshot.status,
            messages: snapshot.messages,
        }
    }
}

impl UiMessage {
    pub fn from_history_items(items: Vec<HistoryItem>) -> Vec<Self> {
        items
            .into_iter()
            .map(|item| match item {
                HistoryItem::Message(message) => Self::Message(message),
                HistoryItem::Display(display) => Self::Display(display),
                HistoryItem::Plan(plan) => Self::ProposedPlan {
                    text: plan.markdown,
                },
                HistoryItem::Summary(summary) => Self::CompactSummary {
                    text: summary.markdown,
                },
            })
            .collect()
    }

    pub fn as_message(&self) -> Option<&Message> {
        match self {
            Self::Message(message) => Some(message),
            Self::Display(_)
            | Self::ProposedPlan { .. }
            | Self::RunDivider { .. }
            | Self::Notification(_)
            | Self::CompactSummary { .. } => None,
        }
    }

    pub fn as_message_mut(&mut self) -> Option<&mut Message> {
        match self {
            Self::Message(message) => Some(message),
            Self::Display(_)
            | Self::ProposedPlan { .. }
            | Self::RunDivider { .. }
            | Self::Notification(_)
            | Self::CompactSummary { .. } => None,
        }
    }
}

/// 底部状态栏展示的信息
#[derive(Debug, Clone)]
pub struct StatusBar {
    /// 当前使用的模型 ID（如 "deepseek-v4-pro"）
    pub model: String,
    /// 思考程度（如果有）
    pub thinking_effort: Option<ThinkingEffort>,
    /// 当前活跃的供应商名称（如 "Bifrost"）
    pub active_provider: String,
    /// 当前工作目录路径
    pub cwd: PathBuf,
    /// 当前运行 profile
    pub active_profile: ActiveProfile,
    /// 最近一次请求的上下文 token 数。
    pub current_context_tokens: i64,
    /// 当前会话历史累计 token 数。
    pub total_tokens: i64,
    /// 当前会话历史累计缓存命中 token 数。
    pub total_cached_tokens: i64,
    /// 当前模型上下文窗口。
    pub context_window: Option<u32>,
    /// 当前 git 分支名（detached HEAD 显示 "detached <sha>"，不在仓库中为 None）。
    pub git_branch: Option<String>,
}

impl Default for StatusBar {
    fn default() -> Self {
        Self {
            model: String::new(),
            thinking_effort: None,
            active_provider: String::new(),
            cwd: PathBuf::new(),
            active_profile: ActiveProfile::Main,
            current_context_tokens: 0,
            total_tokens: 0,
            total_cached_tokens: 0,
            context_window: None,
            git_branch: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum HelpTab {
    #[default]
    General,
    Commands,
    Skills,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelpDrawerState {
    pub tab: HelpTab,
    pub commands: Vec<CommandSummary>,
    pub general_selected: usize,
    pub command_selected: usize,
    pub skill_selected: usize,
}

impl HelpDrawerState {
    pub fn new(commands: Vec<CommandSummary>) -> Self {
        Self {
            tab: HelpTab::General,
            commands,
            general_selected: 0,
            command_selected: 0,
            skill_selected: 0,
        }
    }
}

/// 渲染管线缓存，避免每帧全量重建。
#[derive(Debug, Default)]
pub struct RenderCache {
    // 已完成消息缓存
    pub completed_lines: Vec<Line<'static>>,
    pub completed_selectable: Vec<String>,
    pub completed_message_count: usize,
    pub completed_content_width: usize,
    pub completed_show_thinking: bool,
}

#[derive(Debug)]
pub struct UiState {
    pub messages: Vec<UiMessage>,
    /// 本地 optimistic echo 的一次性 runtime 回显关联表；只存在于当前 TUI 进程内。
    pub pending_client_echoes: HashMap<String, Vec<usize>>,
    /// 正在流式构建中的 assistant 消息（SSE 实时显示）
    pub pending_assistant: Option<Message>,
    /// 正在流式构建中的 proposed plan markdown。
    pub pending_proposed_plan: Option<String>,
    /// 正在流式构建中的 compact 摘要（含呼吸动画，不走缓存）
    pub pending_compact_summary: Option<String>,
    /// 渲染后的消息总行数（用于滚动条计算）
    pub total_lines: usize,
    /// 消息区域的位置和大小
    pub messages_area: Rect,
    /// 当前渲染出的全部消息行纯文本，用于鼠标拖选反查内容。
    pub selectable_message_lines: Vec<String>,
    /// 当前消息视口顶部对应 selectable_message_lines 的行号。
    pub message_scroll_y: usize,
    /// 当前屏幕上可由鼠标拖选复制的文本行。
    pub selectable_screen_lines: Vec<SelectableScreenLine>,
    /// TUI 刚启动、尚未进入任何会话/交互前展示的一次性启动页。
    pub show_start_screen: bool,
    /// 启动时配置的 MCP server 数量，用于首屏项目仪表盘。
    pub startup_mcp_server_count: usize,
    /// 当前项目目录下是否存在非空 AGENTS.md，用于首屏项目仪表盘。
    pub startup_has_project_instructions: bool,
    /// 启动时读取的最近会话，用于首屏提供可恢复的上下文线索。
    pub startup_recent_sessions: Vec<SessionSummary>,
    /// 启动页中文提示，初始化时从静态列表随机选择一次。
    pub startup_tip: String,
    /// 鼠标拖选状态。
    pub text_selection: Option<TextSelection>,
    pub is_selecting_text: bool,
    pub input: String,
    pub input_mentions: Vec<InputMention>,
    pub input_images: Vec<InputImageAttachment>,
    pub input_paste_markers: Vec<InputPasteMarker>,
    pub input_placeholder: String,
    pub input_scroll_line: usize,
    pub input_wrap_width: usize,
    /// 当前 query 运行期间由普通 Enter 暂存在 UI 侧的用户输入。
    pub queued_user_inputs: VecDeque<UserDraft>,
    /// 已提交给 engine、等待当前轮结束后插入历史的用户输入。
    pub pending_intervention_inputs: VecDeque<UserDraft>,
    /// 当前 intervention 请求对应的 optimistic echo token。
    pub pending_intervention_client_echo_id: Option<String>,
    /// 光标偏移量，按 Unicode 字符计数（不是字节）
    pub cursor_char: usize,
    pub agent_status: AgentStatus,
    /// 当前运行状态行优先展示的动态标题，通常从 thinking 内容的首个 Markdown 粗体标题提取。
    pub activity_status_title: Option<String>,
    /// 手动 /compact 命令是否正在执行。compact 不走普通 query 生命周期。
    pub manual_compact_running: bool,
    /// 当前 query 的有效运行计时器；等待用户授权/回答时暂停。
    pub run_timer: Option<RunTimer>,
    /// 从底部向上滚动的行数（0 = 位于底部，显示最新消息）
    pub scroll_offset: usize,
    /// 自动滚动锁定：true = 有新内容时自动保持在底部；false = 用户手动浏览历史不跳转
    pub auto_scroll: bool,
    /// 自适应滚动步长（根据滚动速度动态调整）
    pub scroll_step: usize,
    /// 上次滚动时间戳（用于速度计算）
    pub last_scroll_time: Option<tokio::time::Instant>,
    /// Agent runtime 的 JoinHandle，用于生命周期管理
    pub runtime_handle: Option<tokio::task::JoinHandle<()>>,
    /// 正在运行中的工具 ID 集合（已收到 ToolUse 但尚未收到 ToolResult）
    pub running_tools: HashSet<String>,
    /// 等待用户确认/输入的工具暂停队列，按到达顺序处理。
    pub pending_tool_pauses: VecDeque<ToolPauseRequest>,
    /// 子 agent 视图模型，按 session id 存储完整消息。
    pub subagents: HashMap<String, SubagentNode>,
    /// 父 tool_use_id 到子 agent session id 的映射。
    pub subagents_by_tool_use: HashMap<String, String>,
    /// `messages` 中包含未完成工具（pending tool use）的第一条消息索引；
    /// 该索引及之后的消息不进缓存，每帧重新渲染（与 pending_assistant 同级），
    /// 保证呼吸灯动画实时更新。未完成工具包括：运行中的 subagent、
    /// 尚未收到 ToolResult 的普通工具（bash / read / edit 等）。
    /// `usize::MAX` 表示无未完成工具，所有已完成消息均可缓存。
    pub live_message_start: usize,
    /// 尚未完成的 tool_use_id → 所在消息索引的映射。
    /// ToolUse 事件到达时暂存于 `pending_assistant`（未入 messages），不在此 map 中；
    /// 当 `pending_assistant` 在 TurnEnded/RunFinished 提交到 messages 时才写入。
    /// ToolResult 到达时从 map 中移除。用于 O(1) 定位 pending tool 所在消息，
    /// 避免每次更新 live 边界时全量扫描消息列表。
    pub pending_tool_message_map: std::collections::HashMap<String, usize>,
    /// 权限抽屉当前选中的操作：0 = Yes, 1 = No。
    pub permission_selected: usize,
    /// 用户问题抽屉当前题目索引。
    pub user_input_question_index: usize,
    /// 用户问题抽屉每题当前选项索引。
    pub user_input_selected: Vec<usize>,
    /// 用户问题抽屉每题是否已确认答案。
    pub user_input_answered: Vec<bool>,
    /// 用户问题抽屉是否正在编辑当前问题 note。
    pub user_input_note_mode: bool,
    /// 用户问题抽屉每题 note。
    pub user_input_notes: Vec<String>,
    /// 用户问题抽屉每题 note 光标偏移量，按 Unicode 字符计数。
    pub user_input_note_cursors: Vec<usize>,
    /// 权限抽屉从底部向上滚动的行数（0 = 位于底部）。
    pub permission_scroll_offset: usize,
    /// 当前权限抽屉整体区域，用于鼠标事件命中判断。
    pub permission_drawer_area: Rect,
    /// 当前权限抽屉可滚动内容区域，用于鼠标滚动命中判断。
    pub permission_drawer_body_area: Rect,
    /// 当前权限抽屉内容总行数。
    pub permission_drawer_content_len: usize,
    /// 底部状态栏信息
    pub status_bar: StatusBar,
    /// 命令自动补全
    pub autocomplete: CommandAutocomplete,
    /// @ mention 自动补全
    pub mention_autocomplete: MentionAutocomplete,
    /// 当前会话标题（显示在头部栏）
    pub current_session_title: Option<String>,
    /// 当前会话 ID（新建或切换时设置）
    pub current_session_id: Option<String>,
    /// 待处理的交互请求（非 None 时渲染选择页）
    pub interaction_request: Option<InteractionRequest>,
    /// 交互选择页的当前步骤与选中索引（TUI 本地状态）
    pub interaction_step: Option<InteractionStep>,
    /// /help 底部抽屉状态。
    pub help_drawer: Option<HelpDrawerState>,
    /// 是否在消息区展示 thinking 块。
    pub show_thinking_blocks: bool,
    /// 渲染管线缓存，避免流式期间每帧全量重建。
    pub render_cache: RenderCache,
    /// 待审批的计划。
    pub plan_approval: Option<SubmittedPlan>,
    /// 计划审批抽屉当前选中的操作。
    pub plan_approval_selected: usize,
    /// 计划审批抽屉是否使用 Auto 模式执行。
    pub plan_approval_auto: bool,
}

impl Default for UiState {
    fn default() -> Self {
        Self::new()
    }
}

impl UiState {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
            pending_client_echoes: HashMap::new(),
            pending_assistant: None,
            pending_proposed_plan: None,
            pending_compact_summary: None,
            total_lines: 0,
            messages_area: Rect::default(),
            selectable_message_lines: Vec::new(),
            message_scroll_y: 0,
            selectable_screen_lines: Vec::new(),
            show_start_screen: true,
            startup_mcp_server_count: 0,
            startup_has_project_instructions: false,
            startup_recent_sessions: Vec::new(),
            startup_tip: pick_start_screen_tip(),
            text_selection: None,
            is_selecting_text: false,
            input: String::new(),
            input_mentions: Vec::new(),
            input_images: Vec::new(),
            input_paste_markers: Vec::new(),
            input_placeholder: pick_input_placeholder(),
            input_scroll_line: 0,
            input_wrap_width: DEFAULT_INPUT_WRAP_WIDTH,
            queued_user_inputs: VecDeque::new(),
            pending_intervention_inputs: VecDeque::new(),
            pending_intervention_client_echo_id: None,
            cursor_char: 0,
            agent_status: AgentStatus::Idle,
            activity_status_title: None,
            manual_compact_running: false,
            run_timer: None,
            scroll_offset: 0,
            auto_scroll: true,
            scroll_step: 1,
            last_scroll_time: None,
            runtime_handle: None,
            running_tools: HashSet::new(),
            pending_tool_pauses: VecDeque::new(),
            subagents: HashMap::new(),
            subagents_by_tool_use: HashMap::new(),
            live_message_start: usize::MAX,
            pending_tool_message_map: std::collections::HashMap::new(),
            permission_selected: 0,
            user_input_question_index: 0,
            user_input_selected: Vec::new(),
            user_input_answered: Vec::new(),
            user_input_note_mode: false,
            user_input_notes: Vec::new(),
            user_input_note_cursors: Vec::new(),
            permission_scroll_offset: 0,
            permission_drawer_area: Rect::default(),
            permission_drawer_body_area: Rect::default(),
            permission_drawer_content_len: 0,
            status_bar: StatusBar::default(),
            autocomplete: CommandAutocomplete {
                all_commands: crate::command::builtin_command_summaries(),
                ..CommandAutocomplete::default()
            },
            mention_autocomplete: MentionAutocomplete::default(),
            current_session_title: None,
            current_session_id: None,
            interaction_request: None,
            interaction_step: None,
            help_drawer: None,
            show_thinking_blocks: true,
            render_cache: RenderCache::default(),
            plan_approval: None,
            plan_approval_selected: 0,
            plan_approval_auto: false,
        }
    }

    fn refresh_input_placeholder(&mut self) {
        self.input_placeholder = pick_input_placeholder();
    }

    pub fn active_tool_pause(&self) -> Option<&ToolPauseRequest> {
        self.pending_tool_pauses.front()
    }

    pub fn push_tool_pause(&mut self, req: ToolPauseRequest) -> bool {
        if let Some(existing) = self
            .pending_tool_pauses
            .iter_mut()
            .find(|pause| pause.tool_use_id == req.tool_use_id)
        {
            *existing = req;
            return false;
        }

        let was_empty = self.pending_tool_pauses.is_empty();
        self.pending_tool_pauses.push_back(req);
        was_empty
    }

    pub fn remove_tool_pause(&mut self, tool_use_id: &str) -> bool {
        let removed_active = self
            .active_tool_pause()
            .is_some_and(|pause| pause.tool_use_id == tool_use_id);
        self.pending_tool_pauses
            .retain(|pause| pause.tool_use_id != tool_use_id);
        removed_active
    }

    pub fn tool_pause_for_tool_use(&self, tool_use_id: &str) -> Option<&ToolPauseRequest> {
        self.pending_tool_pauses.iter().find(|pause| {
            pause.source_agent_label.is_none() && pause_preview_tool_use_id(pause) == tool_use_id
        })
    }

    pub fn is_active_tool_pause(&self, pause: &ToolPauseRequest) -> bool {
        self.active_tool_pause()
            .is_some_and(|active| active.tool_use_id == pause.tool_use_id)
    }

    pub fn finish_tool_pause_removal(&mut self, removed_active: bool) {
        if self.pending_tool_pauses.is_empty() {
            self.resume_run_timer();
            self.reset_permission_drawer();
            if self.agent_status == AgentStatus::AwaitingInput {
                self.agent_status = AgentStatus::Working;
            }
        } else if removed_active {
            self.prepare_active_tool_pause();
            self.agent_status = AgentStatus::AwaitingInput;
        }
    }

    pub fn open_plan_approval(&mut self, plan: SubmittedPlan) {
        if self
            .plan_approval
            .as_ref()
            .is_some_and(|current| current.id == plan.id)
        {
            return;
        }
        self.plan_approval = Some(plan);
        self.plan_approval_selected = 0;
        self.plan_approval_auto = false;
        if self.auto_scroll {
            self.scroll_offset = 0;
        }
    }

    pub fn clear_resolved_plan_approval(&mut self, plan_id: &str) {
        if self
            .plan_approval
            .as_ref()
            .is_some_and(|plan| plan.id == plan_id)
        {
            self.clear_plan_approval();
        }
    }

    pub fn clear_plan_approval(&mut self) {
        self.plan_approval = None;
        self.plan_approval_selected = 0;
        self.plan_approval_auto = false;
    }

    pub fn start_run_timer(&mut self) {
        self.run_timer = Some(RunTimer::started_at(Instant::now()));
    }

    pub fn sync_run_timer(&mut self, elapsed: Duration, paused: bool) {
        self.run_timer = Some(RunTimer::started_with_elapsed_at(
            Instant::now(),
            elapsed,
            paused,
        ));
    }

    pub fn begin_manual_compact(&mut self) {
        self.manual_compact_running = true;
        self.agent_status = AgentStatus::Working;
        if self.run_timer.is_none() {
            self.start_run_timer();
        }
    }

    pub fn finish_manual_compact(&mut self) {
        if !self.manual_compact_running {
            return;
        }
        self.manual_compact_running = false;
        self.run_timer = None;
        self.agent_status = AgentStatus::Idle;
    }

    pub fn clear_run_dividers(&mut self) {
        let before = self.messages.len();
        self.messages
            .retain(|message| !matches!(message, UiMessage::RunDivider { .. }));
        if self.messages.len() != before {
            self.invalidate_completed_cache();
            // RunDivider 被移除后消息索引发生偏移，需要重建 map 和边界
            self.rebuild_pending_tool_map();
        }
    }

    pub fn pause_run_timer(&mut self) {
        if let Some(timer) = &mut self.run_timer {
            timer.pause_at(Instant::now());
        }
    }

    pub fn resume_run_timer(&mut self) {
        if let Some(timer) = &mut self.run_timer {
            timer.resume_at(Instant::now());
        }
    }

    pub fn finish_run_timer(&mut self) -> Option<Duration> {
        self.run_timer
            .take()
            .map(|timer| timer.finish_at(Instant::now()))
    }

    pub fn current_run_elapsed(&self) -> Option<Duration> {
        self.run_timer
            .as_ref()
            .map(|timer| timer.elapsed_at(Instant::now()))
    }

    pub fn apply_runtime_status_sync(&mut self, status: omini_protocol::SessionRuntimeStatus) {
        let omini_protocol::SessionRuntimeStatus {
            active_profile,
            pending_plan_approval,
            subagent_sessions,
            activity,
            state,
            pending_pauses,
            git_branch,
            ..
        } = status;

        self.status_bar.active_profile = match active_profile {
            omini_protocol::ActiveProfile::Main => ActiveProfile::Main,
            omini_protocol::ActiveProfile::Auto => ActiveProfile::Auto,
            omini_protocol::ActiveProfile::Plan => ActiveProfile::Plan,
        };
        self.status_bar.git_branch = git_branch;
        self.mention_autocomplete
            .set_candidates(agent_summaries_to_mention_candidates(subagent_sessions));
        self.update_input_autocomplete();
        self.sync_pending_plan_approval(pending_plan_approval);

        let Some(activity) = activity else {
            return;
        };

        let agent_status = match state {
            omini_protocol::SessionRuntimeState::Idle => return,
            omini_protocol::SessionRuntimeState::Thinking => AgentStatus::Thinking,
            omini_protocol::SessionRuntimeState::Waiting => AgentStatus::AwaitingInput,
            omini_protocol::SessionRuntimeState::Working
            | omini_protocol::SessionRuntimeState::Compacting => AgentStatus::Working,
        };
        // RuntimeStatus 是连接同步事实；compact 可能由其它客户端发起，不能标记为本地 manual compact。
        let paused = activity.kind == omini_protocol::SessionRuntimeActivityKind::Query
            && (state == omini_protocol::SessionRuntimeState::Waiting
                || !pending_pauses.is_empty());

        self.manual_compact_running = false;
        self.sync_run_timer(Duration::from_millis(activity.elapsed_ms), paused);
        self.agent_status = agent_status;
    }

    fn sync_pending_plan_approval(&mut self, pending: Option<omini_protocol::PlanSubmittedEvent>) {
        match pending {
            Some(plan) => self.open_plan_approval(SubmittedPlan {
                id: plan.plan_id,
                title: plan.title,
                markdown: plan.markdown,
                path: PathBuf::new(),
                created_at: chrono::Utc::now(),
            }),
            None => self.clear_plan_approval(),
        }
    }

    pub fn is_run_timer_paused(&self) -> bool {
        self.run_timer.as_ref().is_some_and(RunTimer::is_paused)
    }

    pub fn clear_selectable_screen_lines(&mut self) {
        self.selectable_screen_lines.clear();
    }

    pub fn register_selectable_screen_line(
        &mut self,
        row: u16,
        col: u16,
        width: u16,
        text: String,
    ) {
        if width == 0 {
            return;
        }
        self.selectable_screen_lines.push(SelectableScreenLine {
            row,
            col,
            width,
            text,
        });
    }

    /// 使已完成消息的渲染缓存失效（消息列表变更、resize、thinking 切换时调用）。
    pub fn invalidate_completed_cache(&mut self) {
        self.render_cache.completed_message_count = 0;
    }

    /// 扫描指定消息，将其中的 ToolUse 块注册到 `pending_tool_message_map`。
    /// 在 `pending_assistant` 提交到 `messages` 时（TurnEnded / RunFinished）调用。
    fn populate_pending_tool_map_from_message(&mut self, msg_idx: usize) {
        let Some(message) = self.messages[msg_idx].as_message() else {
            return;
        };
        // 收集该消息中已有的 ToolResult id（同一消息内可能 Text → ToolUse → ToolResult）
        let mut resolved_ids = std::collections::HashSet::new();
        for block in &message.content {
            if let omini_domain::message::ContentBlock::ToolResult(tr) = block {
                resolved_ids.insert(&tr.tool_use_id);
            }
        }
        for block in &message.content {
            if let omini_domain::message::ContentBlock::ToolUse(tu) = block
                && !resolved_ids.contains(&tu.id)
            {
                self.pending_tool_message_map.insert(tu.id.clone(), msg_idx);
            }
        }
    }

    /// 全量重建 `pending_tool_message_map` 并重算 `live_message_start`。
    /// 仅在 `apply_session_snapshot`（中途连接 / 切换会话）时调用，O(n)。
    fn rebuild_pending_tool_map(&mut self) {
        self.pending_tool_message_map.clear();
        for (msg_idx, ui_msg) in self.messages.iter().enumerate() {
            let Some(message) = ui_msg.as_message() else {
                continue;
            };
            let mut resolved_ids = std::collections::HashSet::new();
            for block in &message.content {
                if let omini_domain::message::ContentBlock::ToolResult(tr) = block {
                    resolved_ids.insert(&tr.tool_use_id);
                }
            }
            for block in &message.content {
                if let omini_domain::message::ContentBlock::ToolUse(tu) = block
                    && !resolved_ids.contains(&tu.id)
                {
                    self.pending_tool_message_map.insert(tu.id.clone(), msg_idx);
                }
            }
        }
        // 重算边界：map 中最小 msg_idx 与首个 running subagent 的较小值
        let map_min = self
            .pending_tool_message_map
            .values()
            .copied()
            .min()
            .unwrap_or(usize::MAX);
        let subagent_min = self.find_earliest_running_subagent_from(0);
        self.live_message_start = map_min.min(subagent_min);
    }

    /// 工具结果到达时的增量更新。
    ///
    /// 若该 tool_use 在 map 中（即已提交到 messages），移除后检查该消息是否还有
    /// 其他 pending tool；若没有，从 `live_message_start` 向后扫到下一个含 pending
    /// 工具的消息。正常流程中 pending tool 总在尾部，扫描距离 k 极小，O(k)。
    pub fn on_tool_result(&mut self, tool_use_id: &str) {
        let Some(msg_idx) = self.pending_tool_message_map.remove(tool_use_id) else {
            return;
        };
        let still_has_pending = self.message_has_pending_tools(msg_idx);
        if !still_has_pending && msg_idx == self.live_message_start {
            // 当前边界消息已无 pending tool，向后扫描
            let mut new_start = usize::MAX;
            for i in (msg_idx + 1)..self.messages.len() {
                if self.message_has_pending_tools(i) || self.message_has_running_subagent(i) {
                    new_start = i;
                    break;
                }
            }
            // 也检查后面是否有 running subagent（它们不在 map 中）
            let sub_min = self.find_earliest_running_subagent_from(msg_idx + 1);
            new_start = new_start.min(sub_min);
            self.live_message_start = new_start;
        }
        // 若该消息仍有 pending tool 或不在边界，边界不变
    }

    /// 检查指定消息是否仍有 pending tool（在 map 中或有 running subagent）。
    fn message_has_pending_tools(&self, msg_idx: usize) -> bool {
        let Some(message) = self.messages[msg_idx].as_message() else {
            return false;
        };
        for block in &message.content {
            if let omini_domain::message::ContentBlock::ToolUse(tu) = block {
                if self.pending_tool_message_map.contains_key(&tu.id) {
                    return true;
                }
                if tu.name == "subagent"
                    && self
                        .subagents_by_tool_use
                        .get(&tu.id)
                        .and_then(|sid| self.subagents.get(sid))
                        .is_some_and(|node| {
                            matches!(node.status, crate::types::events::SubagentStatus::Running)
                        })
                {
                    return true;
                }
            }
        }
        false
    }

    /// 检查指定消息是否包含运行中的 subagent。
    fn message_has_running_subagent(&self, msg_idx: usize) -> bool {
        let Some(message) = self.messages[msg_idx].as_message() else {
            return false;
        };
        for block in &message.content {
            if let omini_domain::message::ContentBlock::ToolUse(tu) = block
                && tu.name == "subagent"
                && self
                    .subagents_by_tool_use
                    .get(&tu.id)
                    .and_then(|sid| self.subagents.get(sid))
                    .is_some_and(|node| {
                        matches!(node.status, crate::types::events::SubagentStatus::Running)
                    })
            {
                return true;
            }
        }
        false
    }

    /// 从 `start_idx` 开始找第一个含 running subagent 的消息索引。
    fn find_earliest_running_subagent_from(&self, start_idx: usize) -> usize {
        let has_running = self
            .subagents
            .values()
            .any(|node| matches!(node.status, crate::types::events::SubagentStatus::Running));
        if !has_running {
            return usize::MAX;
        }
        for i in start_idx..self.messages.len() {
            if self.message_has_running_subagent(i) {
                return i;
            }
        }
        usize::MAX
    }

    /// 刷新 live 边界，确保缓存尾部不超过新边界。
    ///
    /// 主要用于 subagent 状态变化（`SubagentStarted` / `SubagentFinished`）等
    /// 不影响 `pending_tool_message_map` 的事件；普通工具完成请优先使用 `on_tool_result`。
    pub fn update_live_boundary(&mut self) {
        let map_min = self
            .pending_tool_message_map
            .values()
            .copied()
            .min()
            .unwrap_or(usize::MAX);
        let sub_min = self.find_earliest_running_subagent_from(self.live_message_start);
        let new_start = map_min.min(sub_min);
        if new_start == self.live_message_start {
            return;
        }
        self.live_message_start = new_start;
        // 边界前进（new_start < cached）时，缓存尾部超出新边界。
        // 将 completed_message_count 缩小到 new_start，下次渲染时
        // completed_message_count == live_start，走缓存命中 + 增量追加分支，
        // 无需全量重建——messages[..new_start] 的内容未变，缓存仍然有效。
        let cached = self.render_cache.completed_message_count;
        if cached > new_start {
            self.render_cache.completed_message_count = new_start;
        }
    }
}

fn pick_input_placeholder() -> String {
    let mut rng = rand::thread_rng();
    let idx = rng.gen_range(0..INPUT_PLACEHOLDERS.len());
    INPUT_PLACEHOLDERS[idx].to_string()
}

fn pick_start_screen_tip() -> String {
    let mut rng = rand::thread_rng();
    let idx = rng.gen_range(0..START_SCREEN_TIPS.len());
    START_SCREEN_TIPS[idx].to_string()
}

#[cfg(test)]
mod tests;
