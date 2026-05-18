use crate::types::config::ThinkingEffort;
use crate::types::display::{
    DisplayImageAttachment, DisplayMention, DisplayMessage, HistoryItem, MentionKind, UserDraft,
};
use crate::types::events::{
    InteractionRequest, RuntimeToUiEvent, SubagentSnapshot, SubagentStatus, ToolPauseKind,
    ToolPauseRequest,
};
use crate::types::message::{ContentBlock, Message, Role, ToolResultBlock};
use ratatui::layout::Rect;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use unicode_width::UnicodeWidthChar;

mod autocomplete;
mod interaction;
mod mention;
mod permission;

pub use autocomplete::CommandAutocomplete;
pub use interaction::{InteractionStep, ModelSelectionEntry};
pub use mention::{InputMention, MentionAutocomplete, MentionCandidate, load_mention_candidates};

pub const PASTE_MARKER_THRESHOLD_CHARS: usize = 512;
pub const PASTE_MARKER_THRESHOLD_NEWLINES: usize = 2;
pub const MAX_INPUT_VISIBLE_LINES: usize = 3;
const DEFAULT_INPUT_WRAP_WIDTH: usize = 80;
const INPUT_PROMPT_WIDTH: usize = 2;

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
pub enum UiMessage {
    Message(Message),
    Display(DisplayMessage),
    Notice { text: String },
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
            })
            .collect()
    }

    pub fn as_message(&self) -> Option<&Message> {
        match self {
            Self::Message(message) => Some(message),
            Self::Display(_) | Self::Notice { .. } | Self::Warning { .. } | Self::Error { .. } => {
                None
            }
        }
    }

    pub fn as_message_mut(&mut self) -> Option<&mut Message> {
        match self {
            Self::Message(message) => Some(message),
            Self::Display(_) | Self::Notice { .. } | Self::Warning { .. } | Self::Error { .. } => {
                None
            }
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
    pub messages: Vec<UiMessage>,
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
            input_mentions: Vec::new(),
            input_images: Vec::new(),
            input_paste_markers: Vec::new(),
            input_scroll_line: 0,
            input_wrap_width: DEFAULT_INPUT_WRAP_WIDTH,
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

    pub fn take_queued_user_draft(&mut self) -> Option<UserDraft> {
        Self::draft_from_inputs(&mut self.queued_user_inputs)
    }

    pub fn take_queued_user_draft_for_intervention(&mut self) -> Option<UserDraft> {
        if !self.pending_intervention_inputs.is_empty() {
            return None;
        }

        let pending = self.queued_user_inputs.drain(..).collect::<VecDeque<_>>();
        let draft = Self::draft_from_input_iter(pending.iter())?;
        self.pending_intervention_inputs = pending;
        Some(draft)
    }

    fn take_pending_intervention_ui_messages(&mut self) -> Vec<UiMessage> {
        self.pending_intervention_inputs
            .drain(..)
            .map(|draft| match draft.history_item() {
                HistoryItem::Message(message) => UiMessage::Message(message),
                HistoryItem::Display(display) => UiMessage::Display(display),
            })
            .collect()
    }

    fn draft_from_inputs(inputs: &mut VecDeque<UserDraft>) -> Option<UserDraft> {
        if inputs.is_empty() {
            return None;
        }

        let drafts = inputs.drain(..).collect::<Vec<_>>();
        Self::draft_from_input_iter(drafts.iter())
    }

    fn draft_from_input_iter<'a>(inputs: impl Iterator<Item = &'a UserDraft>) -> Option<UserDraft> {
        let drafts = inputs.collect::<Vec<_>>();
        if drafts.is_empty() {
            return None;
        }

        Some(combined_user_draft(&drafts))
    }

    pub fn open_interaction_request(&mut self, req: &InteractionRequest) {
        self.interaction_step = match req {
            InteractionRequest::ModelSelection {
                providers,
                current_provider,
                current_model,
            } => {
                let mut entries: Vec<ModelSelectionEntry> = Vec::new();
                let mut selected = 0;
                let default_thinking = match self.status_bar.thinking_effort {
                    Some(ThinkingEffort::Low) => 1,
                    Some(ThinkingEffort::Medium) => 2,
                    Some(ThinkingEffort::High) => 3,
                    Some(ThinkingEffort::None) | None => 0,
                };
                let mut sorted: Vec<_> = providers.clone().into_iter().collect();
                sorted.sort_by(|a, b| a.0.cmp(&b.0));
                for (provider_key, profile) in &sorted {
                    entries.push(ModelSelectionEntry::ProviderHeader {
                        name: profile.name.clone(),
                    });
                    for model in &profile.models {
                        if *provider_key == *current_provider && model.id == *current_model {
                            selected = entries.len();
                        }
                        entries.push(ModelSelectionEntry::Model {
                            provider_key: provider_key.clone(),
                            model: model.clone(),
                        });
                    }
                }
                Some(InteractionStep::ModelSelection {
                    entries,
                    selected,
                    thinking_idx: default_thinking,
                    active_provider: current_provider.clone(),
                    active_model: current_model.clone(),
                })
            }
            InteractionRequest::SessionSelection { sessions } => {
                let mut sorted = sessions.clone();
                sorted.sort_by(|a, b| b.created_at.cmp(&a.created_at));
                let all_sessions = sorted.clone();
                Some(InteractionStep::Session {
                    sessions: sorted,
                    all_sessions,
                    search: String::new(),
                    selected: 0,
                })
            }
        };
    }

    pub fn apply_event(&mut self, event: RuntimeToUiEvent) {
        match event {
            RuntimeToUiEvent::RunStarted => {
                self.pending_assistant = None;
                self.agent_status = AgentStatus::Thinking;
            }
            RuntimeToUiEvent::UserMessageInjected(item) => {
                let ui_message = match item {
                    HistoryItem::Message(message) => UiMessage::Message(message),
                    HistoryItem::Display(display) => UiMessage::Display(display),
                };
                self.messages.push(ui_message);
                if self.auto_scroll {
                    self.scroll_offset = 0;
                }
            }
            RuntimeToUiEvent::TurnStarted => {
                // 如果上轮还有未提交的 pending_assistant，先推入 messages
                if let Some(msg) = self.pending_assistant.take()
                    && !msg.content.is_empty()
                {
                    self.messages.push(UiMessage::Message(msg));
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
                self.finish_subagent_for_tool_result(&tr);
                self.running_tools.remove(&tr.tool_use_id);
                self.pending_tool_previews.remove(&tr.tool_use_id);
                if self.pending_tool_previews.is_empty() {
                    self.reset_permission_drawer();
                    if self.agent_status == AgentStatus::AwaitingInput {
                        self.agent_status = AgentStatus::Working;
                    }
                }
                // 工具结果异步返回，追加到 pending_assistant 或最后一条消息中
                if let Some(pending) = &mut self.pending_assistant {
                    pending.content.push(ContentBlock::ToolResult(tr));
                } else if let Some(last) = self
                    .messages
                    .iter_mut()
                    .rev()
                    .find_map(UiMessage::as_message_mut)
                {
                    last.content.push(ContentBlock::ToolResult(tr));
                } else {
                    let mut msg = Message::new(Role::Assistant, Vec::new());
                    msg.content.push(ContentBlock::ToolResult(tr));
                    self.messages.push(UiMessage::Message(msg));
                }
            }
            RuntimeToUiEvent::TurnEnded => {
                if let Some(msg) = self.pending_assistant.take()
                    && !msg.content.is_empty()
                {
                    self.messages.push(UiMessage::Message(msg));
                }
                let pending_inputs = self.take_pending_intervention_ui_messages();
                self.messages.extend(pending_inputs);
                if self.auto_scroll {
                    self.scroll_offset = 0;
                }
                self.agent_status = AgentStatus::Working;
            }
            RuntimeToUiEvent::RunFinished => {
                if let Some(msg) = self.pending_assistant.take()
                    && !msg.content.is_empty()
                {
                    self.messages.push(UiMessage::Message(msg));
                }
                self.pending_intervention_inputs.clear();
                if self.auto_scroll {
                    self.scroll_offset = 0;
                }
                self.agent_status = AgentStatus::Idle;
            }
            RuntimeToUiEvent::ToolPauseRequested(req) => {
                self.reset_permission_drawer();
                if let ToolPauseKind::UserInput(preview) = &req.kind {
                    self.prepare_user_input_preview(preview);
                }
                self.pending_tool_previews
                    .insert(req.tool_use_id.clone(), req);
                self.agent_status = AgentStatus::AwaitingInput;
            }
            RuntimeToUiEvent::SubagentStarted(event) => {
                self.subagents_by_tool_use
                    .insert(event.spawn_tool_use_id.clone(), event.session_id.clone());
                self.subagents.insert(
                    event.session_id.clone(),
                    SubagentNode {
                        session_id: event.session_id,
                        parent_session_id: event.parent_session_id,
                        spawn_tool_use_id: event.spawn_tool_use_id,
                        agent_label: event.agent_label,
                        status: SubagentStatus::Running,
                        messages: Vec::new(),
                    },
                );
                self.agent_status = AgentStatus::Working;
            }
            RuntimeToUiEvent::SubagentMessageProduced(event) => {
                if let Some(node) = self.subagents.get_mut(&event.session_id) {
                    node.messages.push(event.message);
                }
            }
            RuntimeToUiEvent::SubagentToolUse(event) => {
                if let Some(node) = self.subagents.get_mut(&event.session_id) {
                    let msg =
                        Message::new(Role::Assistant, vec![ContentBlock::ToolUse(event.tool_use)]);
                    node.messages.push(msg);
                }
            }
            RuntimeToUiEvent::SubagentToolResult(event) => {
                self.running_tools.remove(&event.tool_result.tool_use_id);
                self.pending_tool_previews
                    .remove(&event.tool_result.tool_use_id);
                self.pending_tool_previews.remove(&format!(
                    "{}:{}",
                    event.session_id, event.tool_result.tool_use_id
                ));
                if self.pending_tool_previews.is_empty() {
                    self.reset_permission_drawer();
                    if self.agent_status == AgentStatus::AwaitingInput {
                        self.agent_status = AgentStatus::Working;
                    }
                }
                if let Some(node) = self.subagents.get_mut(&event.session_id) {
                    let msg = Message::new(
                        Role::User,
                        vec![ContentBlock::ToolResult(event.tool_result)],
                    );
                    node.messages.push(msg);
                }
            }
            RuntimeToUiEvent::SubagentFinished(event) => {
                if let Some(node) = self.subagents.get_mut(&event.session_id) {
                    node.status = event.status;
                }
            }
            RuntimeToUiEvent::Error(e) => {
                self.messages.push(UiMessage::Error { text: e });
                self.fail_running_subagents();
                if !self.pending_tool_previews.is_empty() {
                    self.agent_status = AgentStatus::AwaitingInput;
                } else if !self.is_run_active() {
                    self.agent_status = AgentStatus::Idle;
                }
                if self.auto_scroll {
                    self.scroll_offset = 0;
                }
            }
            RuntimeToUiEvent::Warning(text) => {
                self.messages.push(UiMessage::Warning { text });
                if self.auto_scroll {
                    self.scroll_offset = 0;
                }
            }
            // ===== 命令系统事件 =====
            RuntimeToUiEvent::Shutdown => {
                // TUI 主循环检测到此状态后会 break
            }
            RuntimeToUiEvent::CommandNotice(text) => {
                self.messages.push(UiMessage::Notice { text });
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

    fn finish_subagent_for_tool_result(&mut self, result: &ToolResultBlock) {
        let Some(session_id) = self.subagents_by_tool_use.get(&result.tool_use_id) else {
            return;
        };
        let Some(node) = self.subagents.get_mut(session_id) else {
            return;
        };
        if node.status != SubagentStatus::Running {
            return;
        }

        node.status = if result.is_error {
            if result.content.trim() == "Execution cancelled" {
                SubagentStatus::Cancelled
            } else {
                SubagentStatus::Failed
            }
        } else {
            SubagentStatus::Completed
        };
    }

    fn fail_running_subagents(&mut self) {
        for node in self.subagents.values_mut() {
            if node.status == SubagentStatus::Running {
                node.status = SubagentStatus::Failed;
            }
        }
    }

    pub fn apply_session_changed(
        &mut self,
        session_id: Option<String>,
        messages: Vec<HistoryItem>,
        subagents: Vec<SubagentSnapshot>,
    ) {
        self.current_session_id = session_id;
        self.messages = UiMessage::from_history_items(messages);
        self.subagents.clear();
        self.subagents_by_tool_use.clear();
        for subagent in subagents {
            let node = SubagentNode::from(subagent);
            self.subagents_by_tool_use
                .insert(node.spawn_tool_use_id.clone(), node.session_id.clone());
            self.subagents.insert(node.session_id.clone(), node);
        }
        self.pending_assistant = None;
        self.queued_user_inputs.clear();
        self.input.clear();
        self.input_mentions.clear();
        self.input_images.clear();
        self.input_paste_markers.clear();
        self.cursor_char = 0;
        self.input_scroll_line = 0;
        self.agent_status = AgentStatus::Idle;
        self.interaction_step = None;
        self.interaction_request = None;
        self.scroll_to_bottom();
    }

    pub fn char_to_byte(&self, char_idx: usize) -> usize {
        char_to_byte(&self.input, char_idx)
    }

    pub fn set_input_wrap_width(&mut self, width: usize) {
        let width = width.max(1);
        if self.input_wrap_width != width {
            self.input_wrap_width = width;
            self.ensure_input_cursor_visible();
        }
    }

    pub fn insert_char(&mut self, c: char) {
        self.insert_text(&c.to_string());
        if matches!(c, '\'' | '"') {
            self.replace_quoted_absolute_image_path_before_cursor(c);
        }
    }

    pub fn insert_paste(&mut self, text: String) {
        if let Some(path) = self.existing_image_path_from_pasted_text(&text) {
            self.insert_image_attachment(path);
            self.ensure_input_cursor_visible();
            return;
        }

        let full_char_count = text.chars().count();
        let newline_count = text.chars().filter(|ch| *ch == '\n').count();
        if full_char_count > PASTE_MARKER_THRESHOLD_CHARS
            || newline_count >= PASTE_MARKER_THRESHOLD_NEWLINES
        {
            let marker = format!("[Pasted Content {full_char_count} chars]");
            let start = self.cursor_char;
            self.insert_text(&marker);
            let marker_len = marker.chars().count();
            self.input_paste_markers.push(InputPasteMarker {
                start_char: start,
                end_char: start + marker_len,
                marker,
                full_text: text,
                full_char_count,
            });
            self.input_paste_markers
                .sort_by(|a, b| a.start_char.cmp(&b.start_char));
        } else {
            self.insert_text(&text);
        }
        self.ensure_input_cursor_visible();
    }

    pub fn insert_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }

        let byte_idx = self.char_to_byte(self.cursor_char);
        self.input.insert_str(byte_idx, text);
        let inserted_len = text.chars().count();
        self.apply_input_edit(self.cursor_char, 0, inserted_len);
        self.cursor_char += inserted_len;
        self.ensure_input_cursor_visible();
    }

    fn insert_image_attachment(&mut self, path: PathBuf) {
        let start = self.cursor_char;
        let marker = self.next_image_marker();
        let replacement = format!("{marker} ");
        self.insert_text(&replacement);
        self.input_images.push(InputImageAttachment {
            start_char: start,
            end_char: start + replacement.chars().count(),
            marker,
            source_path: path.to_string_lossy().to_string(),
            file_name: path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string(),
        });
        self.input_images
            .sort_by(|a, b| a.start_char.cmp(&b.start_char));
    }

    fn replace_range_with_image_attachment(&mut self, start: usize, end: usize, path: PathBuf) {
        let marker = self.next_image_marker();
        let replacement = format!("{marker} ");
        let start_byte = char_to_byte(&self.input, start);
        let end_byte = char_to_byte(&self.input, end);
        self.input.replace_range(start_byte..end_byte, &replacement);

        let old_len = end.saturating_sub(start);
        let new_len = replacement.chars().count();
        self.apply_input_edit(start, old_len, new_len);
        self.input_images.push(InputImageAttachment {
            start_char: start,
            end_char: start + new_len,
            marker,
            source_path: path.to_string_lossy().to_string(),
            file_name: path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string(),
        });
        self.input_images
            .sort_by(|a, b| a.start_char.cmp(&b.start_char));
        self.cursor_char = start + new_len;
    }

    fn replace_quoted_absolute_image_path_before_cursor(&mut self, quote: char) -> bool {
        if self.cursor_char < 2 {
            return false;
        }

        let chars = self.input.chars().collect::<Vec<_>>();
        let closing_idx = self.cursor_char.saturating_sub(1);
        if chars.get(closing_idx).copied() != Some(quote) {
            return false;
        }

        let Some(opening_idx) = chars[..closing_idx].iter().rposition(|ch| *ch == quote) else {
            return false;
        };
        if opening_idx + 1 == closing_idx {
            return false;
        }

        let path_text = chars[opening_idx + 1..closing_idx]
            .iter()
            .collect::<String>();
        if path_text.contains('\n') || path_text.contains('\r') {
            return false;
        }

        let path = PathBuf::from(&path_text);
        if !path.is_absolute() {
            return false;
        }

        let Some(path) = existing_image_path(path) else {
            return false;
        };

        self.replace_range_with_image_attachment(opening_idx, closing_idx + 1, path);
        true
    }

    fn next_image_marker(&self) -> String {
        format!("[Image#{}]", self.input_images.len() + 1)
    }

    fn existing_image_path_for_target(&self, target: &str) -> Option<PathBuf> {
        let path = PathBuf::from(target);
        let path = if path.is_absolute() {
            path
        } else {
            self.status_bar.cwd.join(path)
        };
        existing_image_path(path)
    }

    fn existing_image_path_from_pasted_text(&self, text: &str) -> Option<PathBuf> {
        let path_text = unquote_single_pasted_path(text)?;
        let path = PathBuf::from(path_text);
        let path = if path.is_absolute() {
            path
        } else {
            self.status_bar.cwd.join(path)
        };
        existing_image_path(path)
    }

    pub fn delete_before(&mut self) {
        if self.cursor_char > 0 {
            if let Some((start, end)) = self.input_atom_before_cursor() {
                self.delete_input_range(start, end);
                self.cursor_char = start;
                self.ensure_input_cursor_visible();
                return;
            }

            self.cursor_char -= 1;
            let byte_idx = self.char_to_byte(self.cursor_char);
            self.input.remove(byte_idx);
            self.apply_input_edit(self.cursor_char, 1, 0);
            self.ensure_input_cursor_visible();
        }
    }

    pub fn delete_after(&mut self) {
        if let Some((start, end)) = self.input_atom_after_cursor() {
            self.delete_input_range(start, end);
            self.cursor_char = start;
            self.ensure_input_cursor_visible();
            return;
        }

        let byte_idx = self.char_to_byte(self.cursor_char);
        if byte_idx < self.input.len() {
            self.input.remove(byte_idx);
            self.apply_input_edit(self.cursor_char, 1, 0);
            self.ensure_input_cursor_visible();
        }
    }

    pub fn cursor_left(&mut self) {
        if let Some((start, _)) = self.input_atom_before_cursor() {
            self.cursor_char = start;
            self.ensure_input_cursor_visible();
            return;
        }

        self.cursor_char = self.cursor_char.saturating_sub(1);
        self.ensure_input_cursor_visible();
    }

    pub fn cursor_right(&mut self) {
        if let Some((_, end)) = self.input_atom_after_cursor() {
            self.cursor_char = end;
            self.ensure_input_cursor_visible();
            return;
        }

        let max_chars = self.input.chars().count();
        if self.cursor_char < max_chars {
            self.cursor_char += 1;
        }
        self.ensure_input_cursor_visible();
    }

    pub fn cursor_home(&mut self) {
        self.cursor_char = 0;
        self.ensure_input_cursor_visible();
    }

    pub fn cursor_end(&mut self) {
        self.cursor_char = self.input.chars().count();
        self.ensure_input_cursor_visible();
    }

    pub fn cursor_up_in_input(&mut self) -> bool {
        let Some((line_idx, col)) = self.input_cursor_line_col() else {
            return false;
        };
        if line_idx == 0 {
            return false;
        }
        self.cursor_char = self.input_line_col_to_char(line_idx - 1, col);
        self.ensure_input_cursor_visible();
        true
    }

    pub fn cursor_down_in_input(&mut self) -> bool {
        let Some((line_idx, col)) = self.input_cursor_line_col() else {
            return false;
        };
        let line_count = self.input_line_count();
        if line_idx + 1 >= line_count {
            return false;
        }
        self.cursor_char = self.input_line_col_to_char(line_idx + 1, col);
        self.ensure_input_cursor_visible();
        true
    }

    pub fn input_line_count(&self) -> usize {
        self.input_visual_lines().len()
    }

    pub fn input_visible_line_count(&self) -> usize {
        self.input_line_count().clamp(1, MAX_INPUT_VISIBLE_LINES)
    }

    pub fn ensure_input_cursor_visible(&mut self) {
        let line_idx = self
            .input_cursor_line_col()
            .map(|(line_idx, _)| line_idx)
            .unwrap_or(0);
        let visible = self.input_visible_line_count();
        if line_idx < self.input_scroll_line {
            self.input_scroll_line = line_idx;
        } else if line_idx >= self.input_scroll_line + visible {
            self.input_scroll_line = line_idx + 1 - visible;
        }
        let max_scroll = self.input_line_count().saturating_sub(visible);
        self.input_scroll_line = self.input_scroll_line.min(max_scroll);
    }

    pub fn update_input_autocomplete(&mut self) {
        self.autocomplete.update(&self.input);
        if self.autocomplete.visible {
            if self.mention_autocomplete.visible {
                self.mention_autocomplete.clear_session_cache();
            }
            self.mention_autocomplete.visible = false;
        } else {
            self.mention_autocomplete
                .update(&self.input, self.cursor_char);
        }
    }

    pub fn set_mention_context(&mut self, cwd: PathBuf, candidates: Vec<MentionCandidate>) {
        self.mention_autocomplete.set_cwd(cwd);
        self.mention_autocomplete.set_candidates(candidates);
        self.update_input_autocomplete();
    }

    pub fn insert_selected_mention(&mut self) -> bool {
        let Some(candidate) = self.mention_autocomplete.selected_candidate().cloned() else {
            return false;
        };
        let start = self.mention_autocomplete.active_start;
        let end = self.mention_autocomplete.active_end;

        if candidate.kind == MentionKind::File
            && let Some(path) = self.existing_image_path_for_target(&candidate.target)
        {
            self.replace_range_with_image_attachment(start, end, path);
            self.mention_autocomplete.visible = false;
            self.mention_autocomplete.clear_session_cache();
            self.autocomplete.visible = false;
            self.ensure_input_cursor_visible();
            return true;
        }

        let display = candidate.insert_display();
        let replacement = format!("{display} ");
        let start_byte = char_to_byte(&self.input, start);
        let end_byte = char_to_byte(&self.input, end);
        self.input.replace_range(start_byte..end_byte, &replacement);

        let old_len = end.saturating_sub(start);
        let new_len = replacement.chars().count();
        self.apply_input_edit(start, old_len, new_len);
        let mention_len = replacement.chars().count();
        self.input_mentions.push(InputMention {
            start_char: start,
            end_char: start + mention_len,
            kind: candidate.kind,
            label: candidate.label,
            target: candidate.target,
            description: candidate.description,
        });
        self.input_mentions
            .sort_by(|a, b| a.start_char.cmp(&b.start_char));
        self.cursor_char = start + new_len;
        self.ensure_input_cursor_visible();
        self.mention_autocomplete.visible = false;
        self.mention_autocomplete.clear_session_cache();
        self.autocomplete.visible = false;
        true
    }

    pub fn expand_selected_mention_directory(&mut self) -> bool {
        let Some(candidate) = self.mention_autocomplete.selected_candidate().cloned() else {
            return false;
        };
        if !candidate.is_directory() {
            return self.insert_selected_mention();
        }

        let start = self.mention_autocomplete.active_start;
        let end = self.mention_autocomplete.active_end;
        let replacement = format!("@{}/", candidate.target.trim_end_matches('/'));
        let start_byte = char_to_byte(&self.input, start);
        let end_byte = char_to_byte(&self.input, end);
        self.input.replace_range(start_byte..end_byte, &replacement);

        let old_len = end.saturating_sub(start);
        let new_len = replacement.chars().count();
        self.apply_input_edit(start, old_len, new_len);
        self.cursor_char = start + new_len;
        self.ensure_input_cursor_visible();
        self.autocomplete.visible = false;
        self.mention_autocomplete
            .update(&self.input, self.cursor_char);
        true
    }

    pub fn cancel_mention_autocomplete(&mut self) {
        self.mention_autocomplete.visible = false;
        self.mention_autocomplete.clear_session_cache();
    }

    pub fn take_input_draft(&mut self) -> Option<UserDraft> {
        if self.input.is_empty() {
            return None;
        }

        let text = std::mem::take(&mut self.input);
        let mentions = std::mem::take(&mut self.input_mentions);
        let images = std::mem::take(&mut self.input_images);
        let paste_markers = std::mem::take(&mut self.input_paste_markers);
        self.cursor_char = 0;
        self.input_scroll_line = 0;
        self.autocomplete.visible = false;
        self.mention_autocomplete.visible = false;
        self.mention_autocomplete.clear_session_cache();

        let (text, mentions) = expand_paste_markers(text, mentions, paste_markers);
        Some(UserDraft {
            text,
            mentions: mentions.iter().map(InputMention::display_mention).collect(),
            images: images
                .iter()
                .map(InputImageAttachment::display_attachment)
                .collect(),
        })
    }

    fn apply_input_edit(&mut self, start: usize, old_len: usize, new_len: usize) {
        let end = start + old_len;
        let delta = new_len as isize - old_len as isize;
        self.input_mentions.retain_mut(|mention| {
            if mention.end_char <= start {
                true
            } else if mention.start_char >= end {
                mention.start_char = shift_char(mention.start_char, delta);
                mention.end_char = shift_char(mention.end_char, delta);
                true
            } else {
                false
            }
        });
        self.input_images.retain_mut(|image| {
            if image.end_char <= start {
                true
            } else if image.start_char >= end {
                image.start_char = shift_char(image.start_char, delta);
                image.end_char = shift_char(image.end_char, delta);
                true
            } else {
                false
            }
        });
        self.input_paste_markers.retain_mut(|marker| {
            if marker.end_char <= start {
                true
            } else if marker.start_char >= end {
                marker.start_char = shift_char(marker.start_char, delta);
                marker.end_char = shift_char(marker.end_char, delta);
                true
            } else {
                false
            }
        });
    }

    fn input_atom_before_cursor(&self) -> Option<(usize, usize)> {
        self.input_mentions
            .iter()
            .map(|mention| (mention.start_char, mention.end_char))
            .chain(
                self.input_images
                    .iter()
                    .map(|image| (image.start_char, image.end_char)),
            )
            .chain(
                self.input_paste_markers
                    .iter()
                    .map(|marker| (marker.start_char, marker.end_char)),
            )
            .filter(|(start, end)| self.cursor_char > *start && self.cursor_char <= *end)
            .max_by_key(|(start, _)| *start)
    }

    fn input_atom_after_cursor(&self) -> Option<(usize, usize)> {
        self.input_mentions
            .iter()
            .map(|mention| (mention.start_char, mention.end_char))
            .chain(
                self.input_images
                    .iter()
                    .map(|image| (image.start_char, image.end_char)),
            )
            .chain(
                self.input_paste_markers
                    .iter()
                    .map(|marker| (marker.start_char, marker.end_char)),
            )
            .filter(|(start, end)| self.cursor_char >= *start && self.cursor_char < *end)
            .min_by_key(|(start, _)| *start)
    }

    pub fn input_cursor_line_col(&self) -> Option<(usize, usize)> {
        let lines = self.input_visual_lines();
        for (line_idx, line) in lines.iter().enumerate() {
            if self.cursor_char >= line.start_char && self.cursor_char <= line.end_char {
                return Some((
                    line_idx,
                    self.input_display_width(line.start_char, self.cursor_char),
                ));
            }
        }
        lines.last().map(|line| {
            (
                lines.len().saturating_sub(1),
                self.input_display_width(line.start_char, line.end_char),
            )
        })
    }

    fn input_line_col_to_char(&self, target_line: usize, target_col: usize) -> usize {
        let Some(line) = self.input_visual_lines().get(target_line).copied() else {
            return self.input.chars().count();
        };

        let mut col = 0usize;
        for (idx, ch) in self
            .input
            .chars()
            .enumerate()
            .skip(line.start_char)
            .take(line.end_char.saturating_sub(line.start_char))
        {
            if col >= target_col {
                return idx;
            }
            col += char_display_width(ch);
        }
        line.end_char
    }

    pub fn input_line_bounds(&self) -> Vec<(usize, usize)> {
        self.input_visual_lines()
            .into_iter()
            .map(|line| (line.start_char, line.end_char))
            .collect()
    }

    pub fn input_visual_lines(&self) -> Vec<InputVisualLine> {
        let chars = self.input.chars().collect::<Vec<_>>();
        if chars.is_empty() {
            return vec![InputVisualLine {
                start_char: 0,
                end_char: 0,
            }];
        }

        let mut lines = Vec::new();
        let mut start = 0usize;
        let mut width = 0usize;
        for (idx, ch) in chars.iter().copied().enumerate() {
            if ch == '\n' {
                lines.push(InputVisualLine {
                    start_char: start,
                    end_char: idx,
                });
                start = idx + 1;
                width = 0;
                continue;
            }

            let char_width = char_display_width(ch);
            let capacity = self.input_visual_line_capacity(lines.len());
            if width > 0 && width + char_width > capacity {
                lines.push(InputVisualLine {
                    start_char: start,
                    end_char: idx,
                });
                start = idx;
                width = 0;
            }
            width += char_width;
        }
        lines.push(InputVisualLine {
            start_char: start,
            end_char: chars.len(),
        });
        lines
    }

    pub fn input_visual_line_prefix_width(&self, line_idx: usize) -> usize {
        let _ = line_idx;
        INPUT_PROMPT_WIDTH
    }

    pub fn input_display_width(&self, start: usize, end: usize) -> usize {
        self.input
            .chars()
            .skip(start)
            .take(end.saturating_sub(start))
            .map(char_display_width)
            .sum()
    }

    fn input_visual_line_capacity(&self, line_idx: usize) -> usize {
        self.input_wrap_width
            .saturating_sub(self.input_visual_line_prefix_width(line_idx))
            .max(1)
    }

    pub fn paste_marker_at(&self, start_char: usize) -> Option<&InputPasteMarker> {
        self.input_paste_markers
            .iter()
            .find(|marker| marker.start_char == start_char)
    }

    pub fn image_at(&self, start_char: usize) -> Option<&InputImageAttachment> {
        self.input_images
            .iter()
            .find(|image| image.start_char == start_char)
    }

    pub fn mention_at(&self, start_char: usize) -> Option<&InputMention> {
        self.input_mentions
            .iter()
            .find(|mention| mention.start_char == start_char)
    }

    fn delete_input_range(&mut self, start: usize, end: usize) {
        let start_byte = char_to_byte(&self.input, start);
        let end_byte = char_to_byte(&self.input, end);
        self.input.replace_range(start_byte..end_byte, "");
        self.apply_input_edit(start, end.saturating_sub(start), 0);
        self.ensure_input_cursor_visible();
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

fn char_to_byte(input: &str, char_idx: usize) -> usize {
    input.chars().take(char_idx).map(char::len_utf8).sum()
}

fn shift_char(value: usize, delta: isize) -> usize {
    if delta.is_negative() {
        value.saturating_sub(delta.unsigned_abs())
    } else {
        value.saturating_add(delta as usize)
    }
}

fn char_display_width(ch: char) -> usize {
    UnicodeWidthChar::width(ch).unwrap_or(0)
}

fn unquote_single_pasted_path(text: &str) -> Option<&str> {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.contains('\n') || trimmed.contains('\r') {
        return None;
    }

    if let Some(stripped) = trimmed
        .strip_prefix('\'')
        .and_then(|value| value.strip_suffix('\''))
    {
        return Some(stripped);
    }
    if let Some(stripped) = trimmed
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
    {
        return Some(stripped);
    }
    Some(trimmed)
}

fn existing_image_path(path: PathBuf) -> Option<PathBuf> {
    if path.is_file() && is_supported_image_path(&path) {
        Some(path)
    } else {
        None
    }
}

fn is_supported_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "webp" | "gif"
            )
        })
        .unwrap_or(false)
}

fn expand_paste_markers(
    text: String,
    mut mentions: Vec<InputMention>,
    mut markers: Vec<InputPasteMarker>,
) -> (String, Vec<InputMention>) {
    if markers.is_empty() {
        return (text, mentions);
    }

    markers.sort_by(|a, b| a.start_char.cmp(&b.start_char));
    mentions.sort_by(|a, b| a.start_char.cmp(&b.start_char));

    let chars = text.chars().collect::<Vec<_>>();
    let mut expanded = String::new();
    let mut cursor = 0usize;
    let mut delta: isize = 0;
    let mut marker_iter = markers.iter().peekable();

    for mention in &mut mentions {
        while let Some(marker) = marker_iter.peek() {
            if marker.end_char > mention.start_char {
                break;
            }
            delta += marker.full_char_count as isize - marker.marker.chars().count() as isize;
            marker_iter.next();
        }
        mention.start_char = shift_char(mention.start_char, delta);
        mention.end_char = shift_char(mention.end_char, delta);
    }

    for marker in markers {
        for ch in &chars[cursor..marker.start_char] {
            expanded.push(*ch);
        }
        expanded.push_str(&marker.full_text);
        cursor = marker.end_char;
    }
    for ch in &chars[cursor..] {
        expanded.push(*ch);
    }

    (expanded, mentions)
}

fn combined_user_draft(drafts: &[&UserDraft]) -> UserDraft {
    let mut text = String::new();
    let mut mentions = Vec::new();
    let mut images = Vec::new();
    let mut offset = 0usize;
    for (idx, draft) in drafts.iter().enumerate() {
        if idx > 0 {
            text.push('\n');
            offset += 1;
        }

        mentions.extend(draft.mentions.iter().map(|mention| DisplayMention {
            start_char: mention.start_char + offset,
            end_char: mention.end_char + offset,
            kind: mention.kind,
            label: mention.label.clone(),
            target: mention.target.clone(),
            description: mention.description.clone(),
        }));
        images.extend(draft.images.iter().map(|image| DisplayImageAttachment {
            start_char: image.start_char + offset,
            end_char: image.end_char + offset,
            marker: image.marker.clone(),
            source_path: image.source_path.clone(),
            file_name: image.file_name.clone(),
        }));
        offset += draft.text.chars().count();
        text.push_str(&draft.text);
    }

    UserDraft {
        text,
        mentions,
        images,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::display::MentionKind;
    use crate::types::events::SubagentStartedEvent;

    fn state_with_mention(cursor_char: usize) -> UiState {
        let mut state = UiState::new();
        state.input = "see @src now".to_string();
        state.cursor_char = cursor_char;
        state.input_mentions.push(InputMention {
            start_char: 4,
            end_char: 9,
            kind: MentionKind::Directory,
            label: "src".to_string(),
            target: "src".to_string(),
            description: "directory".to_string(),
        });
        state
    }

    fn long_paste_text() -> String {
        "x".repeat(PASTE_MARKER_THRESHOLD_CHARS + 1)
    }

    fn temp_image_path(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("omini_image_input_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, b"image").unwrap();
        path
    }

    fn start_subagent(state: &mut UiState) {
        state.apply_event(RuntimeToUiEvent::SubagentStarted(SubagentStartedEvent {
            session_id: "sub_1".to_string(),
            parent_session_id: "parent".to_string(),
            spawn_tool_use_id: "tool_1".to_string(),
            agent_label: "explorer".to_string(),
        }));
    }

    #[test]
    fn subagent_spawn_tool_error_finishes_running_state() {
        let mut state = UiState::new();
        start_subagent(&mut state);

        state.apply_event(RuntimeToUiEvent::ToolResult(ToolResultBlock {
            tool_use_id: "tool_1".to_string(),
            is_error: true,
            content: "Stream error: Stream ended unexpectedly".to_string(),
            metadata: None,
        }));

        let node = state.subagents.get("sub_1").unwrap();
        assert_eq!(node.status, SubagentStatus::Failed);
    }

    #[test]
    fn runtime_error_fails_running_subagent_state() {
        let mut state = UiState::new();
        start_subagent(&mut state);

        state.apply_event(RuntimeToUiEvent::Error(
            "Stream error: Stream ended unexpectedly".to_string(),
        ));

        let node = state.subagents.get("sub_1").unwrap();
        assert_eq!(node.status, SubagentStatus::Failed);
    }

    #[test]
    fn backspace_deletes_whole_mention_at_end() {
        let mut state = state_with_mention(9);
        state.delete_before();
        assert_eq!(state.input, "see now");
        assert_eq!(state.cursor_char, 4);
        assert!(state.input_mentions.is_empty());
    }

    #[test]
    fn backspace_deletes_whole_mention_from_inside() {
        let mut state = state_with_mention(6);
        state.delete_before();
        assert_eq!(state.input, "see now");
        assert_eq!(state.cursor_char, 4);
        assert!(state.input_mentions.is_empty());
    }

    #[test]
    fn delete_deletes_whole_mention_at_start() {
        let mut state = state_with_mention(4);
        state.delete_after();
        assert_eq!(state.input, "see now");
        assert_eq!(state.cursor_char, 4);
        assert!(state.input_mentions.is_empty());
    }

    #[test]
    fn delete_deletes_whole_mention_from_inside() {
        let mut state = state_with_mention(6);
        state.delete_after();
        assert_eq!(state.input, "see now");
        assert_eq!(state.cursor_char, 4);
        assert!(state.input_mentions.is_empty());
    }

    #[test]
    fn cursor_left_skips_whole_mention_at_end() {
        let mut state = state_with_mention(9);
        state.cursor_left();
        assert_eq!(state.cursor_char, 4);
    }

    #[test]
    fn cursor_left_skips_whole_mention_from_inside() {
        let mut state = state_with_mention(6);
        state.cursor_left();
        assert_eq!(state.cursor_char, 4);
    }

    #[test]
    fn cursor_right_skips_whole_mention_at_start() {
        let mut state = state_with_mention(4);
        state.cursor_right();
        assert_eq!(state.cursor_char, 9);
    }

    #[test]
    fn cursor_right_skips_whole_mention_from_inside() {
        let mut state = state_with_mention(6);
        state.cursor_right();
        assert_eq!(state.cursor_char, 9);
    }

    #[test]
    fn cursor_movement_in_plain_text_stays_character_based() {
        let mut state = state_with_mention(3);
        state.cursor_left();
        assert_eq!(state.cursor_char, 2);

        state.cursor_char = 9;
        state.cursor_right();
        assert_eq!(state.cursor_char, 10);
    }

    #[test]
    fn inserted_mention_range_includes_trailing_space() {
        let mut state = UiState::new();
        state.input = "@sr".to_string();
        state.cursor_char = 3;
        state.mention_autocomplete.visible = true;
        state.mention_autocomplete.active_start = 0;
        state.mention_autocomplete.active_end = 3;
        state.mention_autocomplete.filtered.push(MentionCandidate {
            kind: MentionKind::Directory,
            label: "src".to_string(),
            target: "src".to_string(),
            description: "directory".to_string(),
        });

        assert!(state.insert_selected_mention());
        assert_eq!(state.input, "@src ");
        assert_eq!(state.cursor_char, 5);
        assert_eq!(state.input_mentions[0].start_char, 0);
        assert_eq!(state.input_mentions[0].end_char, 5);
    }

    #[test]
    fn selected_image_mention_inserts_image_marker() {
        let image = temp_image_path("image.png");
        let cwd = image.parent().unwrap().to_path_buf();
        let mut state = UiState::new();
        state.status_bar.cwd = cwd;
        state.input = "@ima".to_string();
        state.cursor_char = 4;
        state.mention_autocomplete.visible = true;
        state.mention_autocomplete.active_start = 0;
        state.mention_autocomplete.active_end = 4;
        state.mention_autocomplete.filtered.push(MentionCandidate {
            kind: MentionKind::File,
            label: "image.png".to_string(),
            target: "image.png".to_string(),
            description: "file".to_string(),
        });

        assert!(state.insert_selected_mention());
        assert_eq!(state.input, "[Image#1] ");
        assert!(state.input_mentions.is_empty());
        assert_eq!(state.input_images.len(), 1);
        assert_eq!(state.input_images[0].source_path, image.to_string_lossy());
    }

    #[test]
    fn quoted_existing_image_path_paste_inserts_image_marker() {
        let image = temp_image_path("dragged.jpg");
        let mut state = UiState::new();

        state.insert_paste(format!("'{}'", image.display()));

        assert_eq!(state.input, "[Image#1] ");
        assert_eq!(state.input_images.len(), 1);
        assert_eq!(state.input_images[0].source_path, image.to_string_lossy());
    }

    #[test]
    fn nonexistent_image_path_paste_remains_text() {
        let mut state = UiState::new();
        let path = "/tmp/omini_missing_image.png";

        state.insert_paste(format!("'{path}'"));

        assert_eq!(state.input, format!("'{path}'"));
        assert!(state.input_images.is_empty());
    }

    #[test]
    fn typed_quoted_existing_absolute_image_path_inserts_image_marker() {
        let image = temp_image_path("typed.png");
        let mut state = UiState::new();

        for ch in format!("'{}'", image.display()).chars() {
            state.insert_char(ch);
        }

        assert_eq!(state.input, "[Image#1] ");
        assert_eq!(state.input_images.len(), 1);
        assert_eq!(state.input_images[0].source_path, image.to_string_lossy());
    }

    #[test]
    fn typed_quoted_image_path_with_spaces_inserts_image_marker() {
        let image = temp_image_path("typed image.png");
        let mut state = UiState::new();

        for ch in format!("\"{}\"", image.display()).chars() {
            state.insert_char(ch);
        }

        assert_eq!(state.input, "[Image#1] ");
        assert_eq!(state.input_images.len(), 1);
        assert_eq!(state.input_images[0].source_path, image.to_string_lossy());
    }

    #[test]
    fn typed_quoted_nonexistent_image_path_remains_text() {
        let mut state = UiState::new();
        let text = "'/tmp/omini_missing_typed_image.png'";

        for ch in text.chars() {
            state.insert_char(ch);
        }

        assert_eq!(state.input, text);
        assert!(state.input_images.is_empty());
    }

    #[test]
    fn typed_quoted_non_image_path_remains_text() {
        let file = temp_image_path("not-image.txt");
        let mut state = UiState::new();
        let text = format!("'{}'", file.display());

        for ch in text.chars() {
            state.insert_char(ch);
        }

        assert_eq!(state.input, text);
        assert!(state.input_images.is_empty());
    }

    #[test]
    fn typed_at_text_without_selection_remains_plain_text() {
        let mut state = UiState::new();
        for c in "@src ".chars() {
            state.insert_char(c);
            state.update_input_autocomplete();
        }

        assert_eq!(state.input, "@src ");
        assert!(state.input_mentions.is_empty());

        state.cursor_left();
        assert_eq!(state.cursor_char, 4);
        state.delete_before();
        assert_eq!(state.input, "@sr ");
    }

    #[test]
    fn short_paste_inserts_literal_newlines() {
        let mut state = UiState::new();
        state.insert_paste("one\ntwo".to_string());

        assert_eq!(state.input, "one\ntwo");
        assert!(state.input_paste_markers.is_empty());
        assert_eq!(state.input_line_count(), 2);
    }

    #[test]
    fn paste_over_two_lines_inserts_marker_even_when_short() {
        let mut state = UiState::new();
        let pasted = "a\nb\nc".to_string();
        state.insert_paste(pasted.clone());

        assert_eq!(state.input, format!("[Pasted Content {} chars]", 5));
        assert_eq!(state.input_paste_markers.len(), 1);

        let draft = state.take_input_draft().unwrap();
        assert_eq!(draft.text, pasted);
    }

    #[test]
    fn long_paste_inserts_marker_and_submit_expands_original_text() {
        let mut state = UiState::new();
        let pasted = long_paste_text();
        state.insert_paste(pasted.clone());

        assert_eq!(state.input_paste_markers.len(), 1);
        assert_eq!(
            state.input,
            format!(
                "[Pasted Content {} chars]",
                PASTE_MARKER_THRESHOLD_CHARS + 1
            )
        );

        let draft = state.take_input_draft().unwrap();
        assert_eq!(draft.text, pasted);
        assert!(draft.mentions.is_empty());
        assert!(state.input.is_empty());
        assert!(state.input_paste_markers.is_empty());
    }

    #[test]
    fn cursor_skips_whole_paste_marker() {
        let mut state = UiState::new();
        state.insert_paste(long_paste_text());
        let marker_len = state.input.chars().count();

        state.cursor_left();
        assert_eq!(state.cursor_char, 0);

        state.cursor_right();
        assert_eq!(state.cursor_char, marker_len);
    }

    #[test]
    fn delete_removes_whole_paste_marker() {
        let mut state = UiState::new();
        state.insert_paste(long_paste_text());
        state.cursor_home();
        state.delete_after();

        assert!(state.input.is_empty());
        assert!(state.input_paste_markers.is_empty());
    }

    #[test]
    fn backspace_removes_whole_paste_marker() {
        let mut state = UiState::new();
        state.insert_paste(long_paste_text());
        state.delete_before();

        assert!(state.input.is_empty());
        assert!(state.input_paste_markers.is_empty());
        assert_eq!(state.cursor_char, 0);
    }

    #[test]
    fn mention_offsets_shift_after_paste_marker_expansion() {
        let mut state = UiState::new();
        let pasted = long_paste_text();
        state.insert_paste(pasted.clone());
        state.insert_char(' ');
        let mention_start = state.cursor_char;
        state.insert_text("@src ");
        state.input_mentions.push(InputMention {
            start_char: mention_start,
            end_char: mention_start + 5,
            kind: MentionKind::Directory,
            label: "src".to_string(),
            target: "src".to_string(),
            description: "directory".to_string(),
        });

        let draft = state.take_input_draft().unwrap();
        assert_eq!(draft.text, format!("{pasted} @src "));
        assert_eq!(draft.mentions[0].start_char, pasted.chars().count() + 1);
        assert_eq!(draft.mentions[0].end_char, pasted.chars().count() + 6);
    }

    #[test]
    fn input_visible_lines_caps_at_three_and_cursor_scrolls() {
        let mut state = UiState::new();
        state.insert_text("a\nb\nc\nd");

        assert_eq!(state.input_line_count(), 4);
        assert_eq!(state.input_visible_line_count(), 3);
        assert_eq!(state.input_scroll_line, 1);

        assert!(state.cursor_up_in_input());
        assert_eq!(state.input_scroll_line, 1);
        assert!(state.cursor_up_in_input());
        assert_eq!(state.input_scroll_line, 1);
        assert!(state.cursor_up_in_input());
        assert_eq!(state.input_scroll_line, 0);
    }

    #[test]
    fn input_soft_wraps_by_width_without_mutating_text() {
        let mut state = UiState::new();
        state.set_input_wrap_width(6);
        state.insert_text("abcdefghi");

        assert_eq!(state.input, "abcdefghi");
        assert_eq!(state.input_line_bounds(), vec![(0, 4), (4, 8), (8, 9)]);
        assert_eq!(state.input_line_count(), 3);
        assert_eq!(state.input_visible_line_count(), 3);
    }

    #[test]
    fn input_soft_wraps_wide_characters_by_display_width() {
        let mut state = UiState::new();
        state.set_input_wrap_width(6);
        state.insert_text("你好吗x");

        assert_eq!(state.input_line_bounds(), vec![(0, 2), (2, 4)]);
        assert_eq!(state.input_display_width(0, 2), 4);
        assert_eq!(state.input_display_width(2, 4), 3);
    }

    #[test]
    fn input_soft_wrap_scrolls_after_three_visible_lines() {
        let mut state = UiState::new();
        state.set_input_wrap_width(6);
        state.insert_text("abcdefghijklmnopqrst");

        assert_eq!(
            state.input_line_bounds(),
            vec![(0, 4), (4, 8), (8, 12), (12, 16), (16, 20)]
        );
        assert_eq!(state.input_visible_line_count(), 3);
        assert_eq!(state.input_scroll_line, 2);
    }

    #[test]
    fn cursor_moves_vertically_across_soft_wrapped_lines() {
        let mut state = UiState::new();
        state.set_input_wrap_width(6);
        state.insert_text("abcdefghijklmnopqrst");

        assert_eq!(state.input_cursor_line_col(), Some((4, 4)));
        assert!(state.cursor_up_in_input());
        assert_eq!(state.input_cursor_line_col(), Some((3, 4)));
        assert_eq!(state.cursor_char, 16);

        assert!(state.cursor_down_in_input());
        assert_eq!(state.input_cursor_line_col(), Some((4, 4)));
        assert_eq!(state.cursor_char, 20);
    }

    #[test]
    fn manual_newlines_remain_real_line_breaks_with_soft_wrap() {
        let mut state = UiState::new();
        state.set_input_wrap_width(6);
        state.insert_text("ab\ncdefghi");

        assert_eq!(state.input, "ab\ncdefghi");
        assert_eq!(state.input_line_bounds(), vec![(0, 2), (3, 7), (7, 10)]);

        let draft = state.take_input_draft().unwrap();
        assert_eq!(draft.text, "ab\ncdefghi");
    }
}
