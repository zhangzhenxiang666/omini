use crate::types::config::ThinkingEffort;
use crate::types::display::{DisplayMention, DisplayMessage, HistoryItem, UserDraft};
use crate::types::events::{
    InteractionRequest, RuntimeToUiEvent, SubagentSnapshot, SubagentStatus, ToolPauseKind,
    ToolPauseRequest,
};
use crate::types::message::{ContentBlock, Message, Role};
use ratatui::layout::Rect;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;

mod autocomplete;
mod interaction;
mod mention;
mod permission;

pub use autocomplete::CommandAutocomplete;
pub use interaction::{InteractionStep, ModelSelectionEntry};
pub use mention::{InputMention, MentionAutocomplete, MentionCandidate, load_mention_candidates};

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
                if let Some(session_id) = self.subagents_by_tool_use.get(&tr.tool_use_id)
                    && let Some(node) = self.subagents.get_mut(session_id)
                    && node.status == SubagentStatus::Running
                    && tr.is_error
                    && tr.content.trim() == "Execution cancelled"
                {
                    node.status = SubagentStatus::Cancelled;
                }
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
        self.agent_status = AgentStatus::Idle;
        self.interaction_step = None;
        self.interaction_request = None;
        self.scroll_to_bottom();
    }

    pub fn char_to_byte(&self, char_idx: usize) -> usize {
        char_to_byte(&self.input, char_idx)
    }

    pub fn insert_char(&mut self, c: char) {
        let byte_idx = self.char_to_byte(self.cursor_char);
        self.input.insert(byte_idx, c);
        self.apply_input_edit(self.cursor_char, 0, 1);
        self.cursor_char += 1;
    }

    pub fn delete_before(&mut self) {
        if self.cursor_char > 0 {
            if let Some((start, end)) = self.mention_before_cursor() {
                self.delete_input_range(start, end);
                self.cursor_char = start;
                return;
            }

            self.cursor_char -= 1;
            let byte_idx = self.char_to_byte(self.cursor_char);
            self.input.remove(byte_idx);
            self.apply_input_edit(self.cursor_char, 1, 0);
        }
    }

    pub fn delete_after(&mut self) {
        if let Some((start, end)) = self.mention_after_cursor() {
            self.delete_input_range(start, end);
            self.cursor_char = start;
            return;
        }

        let byte_idx = self.char_to_byte(self.cursor_char);
        if byte_idx < self.input.len() {
            self.input.remove(byte_idx);
            self.apply_input_edit(self.cursor_char, 1, 0);
        }
    }

    pub fn cursor_left(&mut self) {
        if let Some((start, _)) = self.mention_before_cursor() {
            self.cursor_char = start;
            return;
        }

        self.cursor_char = self.cursor_char.saturating_sub(1);
    }

    pub fn cursor_right(&mut self) {
        if let Some((_, end)) = self.mention_after_cursor() {
            self.cursor_char = end;
            return;
        }

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
        self.cursor_char = 0;
        self.autocomplete.visible = false;
        self.mention_autocomplete.visible = false;
        self.mention_autocomplete.clear_session_cache();

        Some(UserDraft {
            text,
            mentions: mentions.iter().map(InputMention::display_mention).collect(),
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
    }

    fn mention_before_cursor(&self) -> Option<(usize, usize)> {
        self.input_mentions
            .iter()
            .find(|mention| {
                self.cursor_char > mention.start_char && self.cursor_char <= mention.end_char
            })
            .map(|mention| (mention.start_char, mention.end_char))
    }

    fn mention_after_cursor(&self) -> Option<(usize, usize)> {
        self.input_mentions
            .iter()
            .find(|mention| {
                self.cursor_char >= mention.start_char && self.cursor_char < mention.end_char
            })
            .map(|mention| (mention.start_char, mention.end_char))
    }

    fn delete_input_range(&mut self, start: usize, end: usize) {
        let start_byte = char_to_byte(&self.input, start);
        let end_byte = char_to_byte(&self.input, end);
        self.input.replace_range(start_byte..end_byte, "");
        self.apply_input_edit(start, end.saturating_sub(start), 0);
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

fn combined_user_draft(drafts: &[&UserDraft]) -> UserDraft {
    let mut text = String::new();
    let mut mentions = Vec::new();
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
        offset += draft.text.chars().count();
        text.push_str(&draft.text);
    }

    UserDraft { text, mentions }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::display::MentionKind;

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
}
