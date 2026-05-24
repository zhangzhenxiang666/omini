use crate::types::config::ThinkingEffort;
use crate::types::display::{DisplayImageAttachment, DisplayMessage, HistoryItem, UserDraft};
use crate::types::events::{
    ActiveProfile, CommandSummary, InteractionRequest, SubagentSnapshot, SubagentStatus,
    SubmittedPlan, ToolPauseRequest,
};
use crate::types::message::Message;
use ratatui::layout::Rect;
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
pub use mention::{InputMention, MentionAutocomplete, MentionCandidate};

pub const PASTE_MARKER_THRESHOLD_CHARS: usize = 512;
pub const PASTE_MARKER_THRESHOLD_NEWLINES: usize = 2;
pub const MAX_INPUT_VISIBLE_LINES: usize = 3;
const DEFAULT_INPUT_WRAP_WIDTH: usize = 80;

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
    /// Absolute terminal row for the selectable rendered line.
    pub row: usize,
    /// Display column within the selectable rendered line.
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
    Notice { text: String },
    CompactSummary { text: String },
    Warning { text: String },
    Error { text: String },
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
            | Self::Notice { .. }
            | Self::CompactSummary { .. }
            | Self::Warning { .. }
            | Self::Error { .. } => None,
        }
    }

    pub fn as_message_mut(&mut self) -> Option<&mut Message> {
        match self {
            Self::Message(message) => Some(message),
            Self::Display(_)
            | Self::ProposedPlan { .. }
            | Self::RunDivider { .. }
            | Self::Notice { .. }
            | Self::CompactSummary { .. }
            | Self::Warning { .. }
            | Self::Error { .. } => None,
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
    /// 当前 Plan 模式周期内是否已经发送过用户消息。
    pub plan_mode_message_sent: bool,
    /// 最近一次请求的上下文 token 数。
    pub current_context_tokens: i64,
    /// 当前会话历史累计 token 数。
    pub total_tokens: i64,
    /// 当前会话历史累计缓存命中 token 数。
    pub total_cached_tokens: i64,
    /// 当前模型上下文窗口。
    pub context_window: Option<u32>,
}

impl Default for StatusBar {
    fn default() -> Self {
        Self {
            model: String::new(),
            thinking_effort: None,
            active_provider: String::new(),
            cwd: PathBuf::new(),
            active_profile: ActiveProfile::Main,
            plan_mode_message_sent: false,
            current_context_tokens: 0,
            total_tokens: 0,
            total_cached_tokens: 0,
            context_window: None,
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

#[derive(Debug)]
pub struct UiState {
    pub messages: Vec<UiMessage>,
    /// 正在流式构建中的 assistant 消息（SSE 实时显示）
    pub pending_assistant: Option<Message>,
    /// 正在流式构建中的 proposed plan markdown。
    pub pending_proposed_plan: Option<String>,
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
    /// 鼠标拖选状态。
    pub text_selection: Option<TextSelection>,
    pub is_selecting_text: bool,
    pub input: String,
    pub input_mentions: Vec<InputMention>,
    pub input_images: Vec<InputImageAttachment>,
    pub input_paste_markers: Vec<InputPasteMarker>,
    pub input_scroll_line: usize,
    pub input_wrap_width: usize,
    /// 当前 query 运行期间由普通 Enter 暂存在 UI 侧的用户输入。
    pub queued_user_inputs: VecDeque<UserDraft>,
    /// 已提交给 engine、等待当前轮结束后插入历史的用户输入。
    pub pending_intervention_inputs: VecDeque<UserDraft>,
    /// 光标偏移量，按 Unicode 字符计数（不是字节）
    pub cursor_char: usize,
    pub agent_status: AgentStatus,
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
            pending_assistant: None,
            pending_proposed_plan: None,
            total_lines: 0,
            messages_area: Rect::default(),
            selectable_message_lines: Vec::new(),
            message_scroll_y: 0,
            selectable_screen_lines: Vec::new(),
            text_selection: None,
            is_selecting_text: false,
            input: String::new(),
            input_mentions: Vec::new(),
            input_images: Vec::new(),
            input_paste_markers: Vec::new(),
            input_scroll_line: 0,
            input_wrap_width: DEFAULT_INPUT_WRAP_WIDTH,
            queued_user_inputs: VecDeque::new(),
            pending_intervention_inputs: VecDeque::new(),
            cursor_char: 0,
            agent_status: AgentStatus::Idle,
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
            autocomplete: CommandAutocomplete::default(),
            mention_autocomplete: MentionAutocomplete::default(),
            current_session_title: None,
            current_session_id: None,
            interaction_request: None,
            interaction_step: None,
            help_drawer: None,
            plan_approval: None,
            plan_approval_selected: 0,
            plan_approval_auto: false,
        }
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

    pub fn mark_plan_mode_message_sent(&mut self) {
        if self.status_bar.active_profile == ActiveProfile::Plan {
            self.status_bar.plan_mode_message_sent = true;
        }
    }

    pub fn start_run_timer(&mut self) {
        self.run_timer = Some(RunTimer::started_at(Instant::now()));
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
        self.messages
            .retain(|message| !matches!(message, UiMessage::RunDivider { .. }));
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
}

#[cfg(test)]
mod tests;
