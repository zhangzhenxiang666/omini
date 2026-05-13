use crate::types::config::ModelConfig;
use crate::types::config::ThinkingEffort;
use crate::types::events::{
    CommandSummary, InteractionRequest, RuntimeToUiEvent, ToolPauseRequest,
};
use crate::types::message::{ContentBlock, Message, Role};
use ratatui::layout::Rect;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct SelectionPoint {
    pub row: usize,
    pub col: usize,
}

#[derive(Debug, Clone, Default)]
pub struct TextSelection {
    pub start: SelectionPoint,
    pub end: SelectionPoint,
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
    Error(String),
}

impl std::fmt::Display for AgentStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentStatus::Idle => write!(f, "Ready"),
            AgentStatus::Thinking => write!(f, "Thinking"),
            AgentStatus::Working => write!(f, "Working"),
            AgentStatus::AwaitingInput => write!(f, "Waiting for you"),
            AgentStatus::Error(e) => write!(f, "{e}"),
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
}

impl Default for StatusBar {
    fn default() -> Self {
        Self {
            model: String::new(),
            thinking_effort: None,
            active_provider: String::new(),
            cwd: PathBuf::new(),
        }
    }
}

#[derive(Debug)]
pub struct UiState {
    pub messages: Vec<Message>,
    /// 正在流式构建中的 assistant 消息（SSE 实时显示）
    pub pending_assistant: Option<Message>,
    /// 渲染后的消息总行数（用于滚动条计算）
    pub total_lines: usize,
    /// 消息区域的位置和大小
    pub messages_area: Rect,
    /// 当前渲染出的全部消息行纯文本，用于鼠标拖选反查内容。
    pub selectable_message_lines: Vec<String>,
    /// 当前消息视口顶部对应 selectable_message_lines 的行号。
    pub message_scroll_y: usize,
    /// 鼠标拖选状态。
    pub text_selection: Option<TextSelection>,
    pub is_selecting_text: bool,
    pub input: String,
    /// 当前 query 运行期间由普通 Enter 暂存在 UI 侧的用户输入。
    pub queued_user_inputs: VecDeque<String>,
    /// 已提交给 engine、等待当前轮结束后插入历史的用户输入。
    pub pending_intervention_inputs: VecDeque<String>,
    /// 光标偏移量，按 Unicode 字符计数（不是字节）
    pub cursor_char: usize,
    pub agent_status: AgentStatus,
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
    /// 等待用户确认的工具预览，按 tool_use_id 关联到对应工具块。
    pub pending_tool_previews: HashMap<String, ToolPauseRequest>,
    /// 权限抽屉当前选中的操作：0 = Yes, 1 = No。
    pub permission_selected: usize,
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
    /// 当前会话标题（显示在头部栏）
    pub current_session_title: Option<String>,
    /// 当前会话 ID（新建或切换时设置）
    pub current_session_id: Option<String>,
    /// 待处理的交互请求（非 None 时渲染选择页）
    pub interaction_request: Option<InteractionRequest>,
    /// 交互选择页的当前步骤与选中索引（TUI 本地状态）
    pub interaction_step: Option<InteractionStep>,
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
            total_lines: 0,
            messages_area: Rect::default(),
            selectable_message_lines: Vec::new(),
            message_scroll_y: 0,
            text_selection: None,
            is_selecting_text: false,
            input: String::new(),
            queued_user_inputs: VecDeque::new(),
            pending_intervention_inputs: VecDeque::new(),
            cursor_char: 0,
            agent_status: AgentStatus::Idle,
            scroll_offset: 0,
            auto_scroll: true,
            scroll_step: 1,
            last_scroll_time: None,
            runtime_handle: None,
            running_tools: HashSet::new(),
            pending_tool_previews: HashMap::new(),
            permission_selected: 0,
            permission_scroll_offset: 0,
            permission_drawer_area: Rect::default(),
            permission_drawer_body_area: Rect::default(),
            permission_drawer_content_len: 0,
            status_bar: StatusBar::default(),
            autocomplete: CommandAutocomplete::default(),
            current_session_title: None,
            current_session_id: None,
            interaction_request: None,
            interaction_step: None,
        }
    }

    pub fn active_tool_pause(&self) -> Option<&ToolPauseRequest> {
        self.pending_tool_previews
            .values()
            .min_by(|a, b| a.tool_use_id.cmp(&b.tool_use_id))
    }

    pub fn is_run_active(&self) -> bool {
        matches!(
            self.agent_status,
            AgentStatus::Working | AgentStatus::Thinking | AgentStatus::AwaitingInput
        )
    }

    pub fn take_queued_user_message(&mut self) -> Option<Message> {
        Self::message_from_inputs(&mut self.queued_user_inputs)
    }

    pub fn take_queued_user_message_for_intervention(&mut self) -> Option<Message> {
        if !self.pending_intervention_inputs.is_empty() {
            return None;
        }

        let msg = Self::message_from_inputs(&mut self.queued_user_inputs)?;
        self.pending_intervention_inputs = msg
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text(tb) => Some(tb.text.clone()),
                _ => None,
            })
            .collect();
        Some(msg)
    }

    fn take_pending_intervention_message(&mut self) -> Option<Message> {
        Self::message_from_inputs(&mut self.pending_intervention_inputs)
    }

    fn message_from_inputs(inputs: &mut VecDeque<String>) -> Option<Message> {
        if inputs.is_empty() {
            return None;
        }

        let content = inputs.drain(..).map(ContentBlock::from_text).collect();
        Some(Message::new(Role::User, content))
    }

    pub fn reset_permission_drawer(&mut self) {
        self.permission_selected = 0;
        self.permission_scroll_offset = usize::MAX;
        self.permission_drawer_area = Rect::default();
        self.permission_drawer_body_area = Rect::default();
        self.permission_drawer_content_len = 0;
    }

    pub fn permission_select_prev(&mut self) {
        self.permission_selected = self.permission_selected.saturating_sub(1);
    }

    pub fn permission_select_next(&mut self) {
        self.permission_selected = (self.permission_selected + 1).min(1);
    }

    pub fn permission_scroll_up(&mut self, lines: usize) {
        self.permission_scroll_offset = self.permission_scroll_offset.saturating_add(lines);
    }

    pub fn permission_scroll_down(&mut self, lines: usize) {
        let visible = self.permission_drawer_body_area.height as usize;
        let max_scroll = self.permission_drawer_content_len.saturating_sub(visible);
        let capped_offset = self.permission_scroll_offset.min(max_scroll);
        self.permission_scroll_offset = capped_offset.saturating_sub(lines);
    }

    pub fn apply_event(&mut self, event: RuntimeToUiEvent) {
        match event {
            RuntimeToUiEvent::RunStarted => {
                self.pending_assistant = None;
                self.agent_status = AgentStatus::Thinking;
            }
            RuntimeToUiEvent::TurnStarted => {
                // 如果上轮还有未提交的 pending_assistant，先推入 messages
                if let Some(msg) = self.pending_assistant.take()
                    && !msg.content.is_empty()
                {
                    self.messages.push(msg);
                }
                self.agent_status = AgentStatus::Thinking;
            }
            RuntimeToUiEvent::ThinkingDelta(t) => {
                self.agent_status = AgentStatus::Thinking;
                let pending = self
                    .pending_assistant
                    .get_or_insert_with(|| Message::new(Role::Assistant, Vec::new()));
                if let Some(ContentBlock::Thinking(tb)) = pending.content.last_mut() {
                    tb.thinking.push_str(&t);
                } else {
                    pending.content.push(ContentBlock::from_thinking(t));
                }
            }
            RuntimeToUiEvent::TextDelta(t) => {
                self.agent_status = AgentStatus::Working;
                let pending = self
                    .pending_assistant
                    .get_or_insert_with(|| Message::new(Role::Assistant, Vec::new()));
                if let Some(ContentBlock::Text(tb)) = pending.content.last_mut() {
                    tb.text.push_str(&t);
                } else {
                    pending.content.push(ContentBlock::from_text(t));
                }
            }
            RuntimeToUiEvent::ToolUse(tu) => {
                self.running_tools.insert(tu.id.clone());
                let pending = self
                    .pending_assistant
                    .get_or_insert_with(|| Message::new(Role::Assistant, Vec::new()));
                pending.content.push(ContentBlock::ToolUse(tu));
                self.agent_status = AgentStatus::Working;
            }
            RuntimeToUiEvent::ToolResult(tr) => {
                self.running_tools.remove(&tr.tool_use_id);
                self.pending_tool_previews.remove(&tr.tool_use_id);
                if self.pending_tool_previews.is_empty() {
                    self.reset_permission_drawer();
                }
                // 工具结果异步返回，追加到 pending_assistant 或最后一条消息中
                if let Some(pending) = &mut self.pending_assistant {
                    pending.content.push(ContentBlock::ToolResult(tr));
                } else if let Some(last) = self.messages.last_mut() {
                    last.content.push(ContentBlock::ToolResult(tr));
                } else {
                    let mut msg = Message::new(Role::Assistant, Vec::new());
                    msg.content.push(ContentBlock::ToolResult(tr));
                    self.messages.push(msg);
                }
            }
            RuntimeToUiEvent::TurnEnded => {
                if let Some(msg) = self.pending_assistant.take()
                    && !msg.content.is_empty()
                {
                    self.messages.push(msg);
                }
                if let Some(msg) = self.take_pending_intervention_message() {
                    self.messages.push(msg);
                }
                if self.auto_scroll {
                    self.scroll_offset = 0;
                }
                self.agent_status = AgentStatus::Working;
            }
            RuntimeToUiEvent::RunFinished => {
                if let Some(msg) = self.pending_assistant.take()
                    && !msg.content.is_empty()
                {
                    self.messages.push(msg);
                }
                self.pending_intervention_inputs.clear();
                if self.auto_scroll {
                    self.scroll_offset = 0;
                }
                self.agent_status = AgentStatus::Idle;
            }
            RuntimeToUiEvent::ToolPauseRequested(req) => {
                self.reset_permission_drawer();
                self.pending_tool_previews
                    .insert(req.tool_use_id.clone(), req);
                self.agent_status = AgentStatus::AwaitingInput;
            }
            RuntimeToUiEvent::Error(e) => self.agent_status = AgentStatus::Error(e),
            // ===== 命令系统事件 =====
            RuntimeToUiEvent::Shutdown => {
                // TUI 主循环检测到此状态后会 break
            }
            RuntimeToUiEvent::CommandOutput(text) => {
                self.messages.push(Message::from_user_text(text));
                if self.auto_scroll {
                    self.scroll_offset = 0;
                }
            }
            RuntimeToUiEvent::ModelChanged {
                provider,
                model,
                thinking_effort,
            } => {
                self.status_bar.active_provider = provider;
                self.status_bar.model = model;
                self.status_bar.thinking_effort = thinking_effort;
                // 模型切换成功后自动关闭选择弹窗
                self.interaction_step = None;
                self.interaction_request = None;
            }
            RuntimeToUiEvent::SessionTitleChanged { title } => {
                self.current_session_title = title;
            }
            RuntimeToUiEvent::InteractionRequest(req) => {
                self.interaction_request = Some(req);
            }
            RuntimeToUiEvent::CommandList(cmds) => {
                self.autocomplete.all_commands = cmds;
            }
            // SessionChanged 由 TUI 主循环直接处理，此处无需匹配
            RuntimeToUiEvent::SessionChanged { .. } => {}
        }
    }

    pub fn char_to_byte(&self, char_idx: usize) -> usize {
        self.input.chars().take(char_idx).map(char::len_utf8).sum()
    }

    pub fn insert_char(&mut self, c: char) {
        let byte_idx = self.char_to_byte(self.cursor_char);
        self.input.insert(byte_idx, c);
        self.cursor_char += 1;
    }

    pub fn delete_before(&mut self) {
        if self.cursor_char > 0 {
            self.cursor_char -= 1;
            let byte_idx = self.char_to_byte(self.cursor_char);
            self.input.remove(byte_idx);
        }
    }

    pub fn delete_after(&mut self) {
        let byte_idx = self.char_to_byte(self.cursor_char);
        if byte_idx < self.input.len() {
            self.input.remove(byte_idx);
        }
    }

    pub fn cursor_left(&mut self) {
        self.cursor_char = self.cursor_char.saturating_sub(1);
    }

    pub fn cursor_right(&mut self) {
        let max_chars = self.input.chars().count();
        if self.cursor_char < max_chars {
            self.cursor_char += 1;
        }
    }

    pub fn cursor_home(&mut self) {
        self.cursor_char = 0;
    }

    pub fn cursor_end(&mut self) {
        self.cursor_char = self.input.chars().count();
    }

    /// 根据滚动速度动态调整步长
    pub fn update_scroll_step(&mut self, now: tokio::time::Instant) {
        const MIN_STEP: usize = 1;
        const MAX_STEP: usize = 10;
        const ACCEL_MS: u64 = 80; // 间隔 < 80ms → 加速
        const DECEL_MS: u64 = 250; // 间隔 > 250ms → 减速
        const RESET_MS: u64 = 800; // 间隔 > 800ms → 重置为初始值

        if let Some(last) = self.last_scroll_time {
            let elapsed = now.saturating_duration_since(last);
            let ms = elapsed.as_millis() as u64;

            if ms > RESET_MS {
                self.scroll_step = MIN_STEP;
            } else if ms < ACCEL_MS {
                self.scroll_step = (self.scroll_step + 1).min(MAX_STEP);
            } else if ms > DECEL_MS {
                self.scroll_step = (self.scroll_step / 2).max(MIN_STEP);
            }
            // 中间区间：保持当前步长
        } else {
            self.scroll_step = MIN_STEP;
        }
        self.last_scroll_time = Some(now);
    }

    pub fn scroll_up(&mut self, lines: usize) {
        self.scroll_offset = self.scroll_offset.saturating_add(lines);
        self.auto_scroll = false;
    }

    pub fn scroll_down(&mut self, lines: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(lines);
        if self.scroll_offset == 0 {
            self.auto_scroll = true;
        }
    }

    /// 滚动到消息区顶部
    pub fn scroll_to_top(&mut self) {
        self.scroll_offset = usize::MAX;
        self.auto_scroll = false;
    }

    /// 滚动到消息区底部并恢复自动滚动
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
        self.auto_scroll = true;
    }
}
// ===========================================================================
// 命令自动补全
// ===========================================================================

/// 命令自动补全状态。
#[derive(Debug, Clone, Default)]
pub struct CommandAutocomplete {
    /// 是否显示下拉列表
    pub visible: bool,
    /// Runtime 推送的全量命令列表
    pub all_commands: Vec<CommandSummary>,
    /// 经过当前输入过滤后的子集
    pub filtered: Vec<CommandSummary>,
    /// 当前选中的索引
    pub selected: usize,
}

impl CommandAutocomplete {
    /// 根据当前输入更新过滤后的命令列表。
    pub fn update(&mut self, input: &str) {
        if !input.starts_with('/') {
            self.visible = false;
            return;
        }
        self.visible = true;

        let partial = input[1..].to_lowercase();
        self.filtered = self
            .all_commands
            .iter()
            .filter(|cmd| {
                cmd.name.to_lowercase().contains(&partial)
                    || cmd
                        .aliases
                        .iter()
                        .any(|a| a.to_lowercase().contains(&partial))
            })
            .cloned()
            .collect();

        // 修正 selected 不越界
        let max = self.filtered.len().saturating_sub(1);
        self.selected = self.selected.min(max);
    }

    /// 选中当前项（Enter 时调用）。
    pub fn selected_command(&self) -> Option<&CommandSummary> {
        if self.filtered.is_empty() {
            return None;
        }
        self.filtered.get(self.selected)
    }

    pub fn select_next(&mut self) {
        if self.filtered.is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % self.filtered.len();
    }

    pub fn select_prev(&mut self) {
        if self.filtered.is_empty() {
            return;
        }
        self.selected = self.selected.saturating_sub(1);
    }
}
// ===========================================================================
// 交互选择页步骤
// ===========================================================================

/// 交互选择页的当前步骤。
#[derive(Debug, Clone)]
pub enum ModelSelectionEntry {
    /// Provider 标题（不可选中）
    ProviderHeader { name: String },
    /// 某个 provider 下的模型（可选中）
    Model {
        provider_key: String,
        model: ModelConfig,
    },
}

/// 交互选择页的当前步骤。
#[derive(Debug, Clone)]
pub enum InteractionStep {
    /// 模型选择 — 按 provider 分组的扁平列表
    ModelSelection {
        /// 展平后的条目（ProviderHeader + Model 交替）
        entries: Vec<ModelSelectionEntry>,
        /// 当前选中索引，只指向 Model 条目
        selected: usize,
        /// 当前思考程度：0=None 1=Low 2=Medium 3=High
        thinking_idx: usize,
        /// 打开面板时正在使用的 provider key（用于标记 ✔）
        active_provider: String,
        /// 打开面板时正在使用的 model id
        active_model: String,
    },
    /// 会话选择
    Session {
        sessions: Vec<crate::types::events::SessionSummary>,
        /// 原始全量列表（用于过滤后恢复）
        all_sessions: Vec<crate::types::events::SessionSummary>,
        /// 当前搜索关键词
        search: String,
        selected: usize,
    },
}
