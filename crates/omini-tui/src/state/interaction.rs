use crate::types::config::{ModelConfig, ProviderProfile};
use omini_domain::subagents::{AgentDraft, AgentRecord, AgentSourceKind};
use std::collections::HashMap;
use std::path::PathBuf;
use unicode_width::UnicodeWidthChar;

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
    /// 线程选择
    Thread {
        threads: Vec<crate::types::events::ThreadSummary>,
        /// 原始全量列表（用于过滤后恢复）
        all_threads: Vec<crate::types::events::ThreadSummary>,
        /// 当前搜索关键词
        search: String,
        selected: usize,
    },
    /// Agent 管理抽屉
    Agents(Box<AgentManagerState>),
}

#[derive(Debug, Clone)]
pub struct AgentManagerState {
    pub records: Vec<AgentRecord>,
    pub view: AgentManagerView,
    pub selected: usize,
    pub draft: AgentEditorDraft,
    pub tool_selected: usize,
    pub create_scope_selected: usize,
    pub create_method_selected: usize,
    pub detail_action_selected: usize,
    pub edit_action_selected: usize,
    pub model_entries: Vec<AgentModelEntry>,
    pub model_selected: usize,
    pub current_provider: String,
    pub current_model: String,
    pub message: Option<String>,
    pub draft_wrap_width: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentManagerView {
    List,
    Detail(usize),
    EditMenu,
    EditMetadata,
    EditTools,
    EditModel,
    Create(AgentCreateStep),
    Generate,
    Generating(AgentGenerateReturn),
    GeneratedPreview,
    ConfirmDelete(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentGenerateReturn {
    CreateFlow,
    Direct,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentCreateStep {
    Scope,
    Tools,
    Model,
    Method,
    ManualName,
    ManualDescription,
    ManualInstructions,
    GenerateDescription,
}

#[derive(Debug, Clone)]
pub struct AgentEditorDraft {
    pub source_kind: AgentSourceKind,
    pub original_path: Option<PathBuf>,
    pub name: String,
    pub description: String,
    pub instructions: String,
    pub tools: Vec<String>,
    pub disallow_tools: Vec<String>,
    pub model: Option<String>,
    pub field: AgentEditorField,
    pub generated_description: String,
    pub cursor: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentEditorField {
    Name,
    Description,
    Instructions,
    Tools,
    Model,
    GenerateDescription,
}

#[derive(Debug, Clone)]
pub enum AgentModelEntry {
    Inherit,
    ProviderHeader {
        name: String,
    },
    Model {
        provider_key: String,
        model: ModelConfig,
    },
}

impl AgentManagerState {
    pub fn new(
        records: Vec<AgentRecord>,
        providers: HashMap<String, ProviderProfile>,
        current_provider: String,
        current_model: String,
    ) -> Self {
        let model_entries = build_agent_model_entries(&providers);
        let model_selected = model_entries
            .iter()
            .position(|entry| {
                matches!(
                    entry,
                    AgentModelEntry::Model {
                        provider_key,
                        model
                    } if provider_key == &current_provider && model.id == current_model
                )
            })
            .unwrap_or_else(|| {
                model_entries
                    .iter()
                    .position(|entry| matches!(entry, AgentModelEntry::Model { .. }))
                    .unwrap_or(0)
            });
        Self {
            records,
            view: AgentManagerView::List,
            selected: 0,
            draft: AgentEditorDraft::new(AgentSourceKind::Project, None),
            tool_selected: 0,
            create_scope_selected: 0,
            create_method_selected: 0,
            detail_action_selected: 0,
            edit_action_selected: 0,
            model_entries,
            model_selected,
            current_provider,
            current_model,
            message: None,
            draft_wrap_width: 80,
        }
    }

    pub fn refresh_records(&mut self, records: Vec<AgentRecord>) {
        self.records = records;
        self.selected = self.selected.min(self.records.len());
        self.detail_action_selected = 0;
    }

    pub fn start_create(&mut self) {
        self.draft = AgentEditorDraft::new(AgentSourceKind::Project, None);
        self.tool_selected = 0;
        self.create_scope_selected = 0;
        self.create_method_selected = 0;
        self.model_selected = 0;
        self.view = AgentManagerView::Create(AgentCreateStep::Scope);
    }

    pub fn apply_generated(&mut self, source_kind: AgentSourceKind, draft: AgentDraft) {
        self.draft.source_kind = source_kind;
        self.draft.original_path = None;
        self.draft.name = draft.name;
        self.draft.description = draft.description;
        self.draft.instructions = draft.instructions;
        self.draft.tools = draft.tools;
        self.draft.disallow_tools = draft.disallow_tools;
        self.draft.model = draft.model;
        self.draft.field = AgentEditorField::Name;
        self.draft.cursor = self.draft.name.chars().count();
        self.sync_model_selection_to_draft();
        self.view = AgentManagerView::GeneratedPreview;
        self.message = None;
    }

    pub fn start_edit(&mut self, record: AgentRecord) {
        self.draft.source_kind = record.source_kind;
        self.draft.original_path = record.path;
        self.draft.name = record.name;
        self.draft.description = record.description;
        self.draft.instructions = record.instructions;
        self.draft.tools = record.tools;
        self.draft.disallow_tools = record.disallow_tools;
        self.draft.model = record.model;
        self.draft.field = AgentEditorField::Name;
        self.draft.cursor = self.draft.name.chars().count();
        self.tool_selected = 0;
        self.edit_action_selected = 0;
        self.sync_model_selection_to_draft();
        self.view = AgentManagerView::EditMenu;
        self.message = None;
    }

    pub fn start_generating(&mut self, return_to: AgentGenerateReturn) {
        self.view = AgentManagerView::Generating(return_to);
        self.message = None;
    }

    pub fn fail_generation(&mut self, message: String) {
        let return_to = match self.view {
            AgentManagerView::Generating(return_to) => return_to,
            _ => AgentGenerateReturn::Direct,
        };
        self.draft.field = AgentEditorField::GenerateDescription;
        self.move_draft_cursor_to_current_end();
        self.view = match return_to {
            AgentGenerateReturn::CreateFlow => {
                AgentManagerView::Create(AgentCreateStep::GenerateDescription)
            }
            AgentGenerateReturn::Direct => AgentManagerView::Generate,
        };
        self.message = Some(message);
    }

    pub fn current_draft_text(&self) -> &String {
        match self.draft.field {
            AgentEditorField::Name => &self.draft.name,
            AgentEditorField::Description => &self.draft.description,
            AgentEditorField::Instructions => &self.draft.instructions,
            AgentEditorField::GenerateDescription => &self.draft.generated_description,
            AgentEditorField::Tools | AgentEditorField::Model => &self.draft.name,
        }
    }

    fn current_draft_text_mut(&mut self) -> &mut String {
        match self.draft.field {
            AgentEditorField::Name => &mut self.draft.name,
            AgentEditorField::Description => &mut self.draft.description,
            AgentEditorField::Instructions => &mut self.draft.instructions,
            AgentEditorField::GenerateDescription => &mut self.draft.generated_description,
            AgentEditorField::Tools | AgentEditorField::Model => &mut self.draft.name,
        }
    }

    pub fn insert_draft_char(&mut self, ch: char) {
        let cursor = self
            .draft
            .cursor
            .min(self.current_draft_text().chars().count());
        let byte_idx = char_to_byte_idx(self.current_draft_text(), cursor);
        self.current_draft_text_mut().insert(byte_idx, ch);
        self.draft.cursor = cursor + 1;
    }

    pub fn backspace_draft_char(&mut self) {
        let cursor = self
            .draft
            .cursor
            .min(self.current_draft_text().chars().count());
        if cursor == 0 {
            return;
        }
        let start = char_to_byte_idx(self.current_draft_text(), cursor - 1);
        let end = char_to_byte_idx(self.current_draft_text(), cursor);
        self.current_draft_text_mut().replace_range(start..end, "");
        self.draft.cursor = cursor - 1;
    }

    pub fn move_draft_cursor_left(&mut self) {
        self.draft.cursor = self.draft.cursor.saturating_sub(1);
    }

    pub fn move_draft_cursor_right(&mut self) {
        self.draft.cursor = (self.draft.cursor + 1).min(self.current_draft_text().chars().count());
    }

    pub fn move_draft_cursor_to_current_end(&mut self) {
        self.draft.cursor = self.current_draft_text().chars().count();
    }

    pub fn set_draft_wrap_width(&mut self, width: usize) {
        self.draft_wrap_width = width.max(1);
    }

    pub fn current_field_is_multiline(&self) -> bool {
        matches!(
            self.draft.field,
            AgentEditorField::Description
                | AgentEditorField::Instructions
                | AgentEditorField::GenerateDescription
        )
    }

    pub fn move_draft_cursor_up(&mut self) -> bool {
        let text = self.current_draft_text();
        let Some((line_idx, col)) =
            wrapped_text_cursor(text, self.draft.cursor, self.draft_wrap_width)
        else {
            return false;
        };
        if line_idx == 0 {
            return false;
        }
        self.draft.cursor =
            wrapped_text_line_col_to_char(text, self.draft_wrap_width, line_idx - 1, col);
        true
    }

    pub fn move_draft_cursor_down(&mut self) -> bool {
        let text = self.current_draft_text();
        let lines = wrapped_text_lines(text, self.draft_wrap_width);
        let Some((line_idx, col)) = wrapped_text_cursor_from_lines(text, self.draft.cursor, &lines)
        else {
            return false;
        };
        if line_idx + 1 >= lines.len() {
            return false;
        }
        self.draft.cursor =
            wrapped_text_line_col_to_char_from_lines(text, line_idx + 1, col, &lines);
        true
    }

    pub fn cycle_field(&mut self) {
        let include_config_fields = self.draft.original_path.is_some();
        self.draft.field = match self.draft.field {
            AgentEditorField::Name => AgentEditorField::Description,
            AgentEditorField::Description => AgentEditorField::Instructions,
            AgentEditorField::Instructions if include_config_fields => AgentEditorField::Tools,
            AgentEditorField::Instructions => AgentEditorField::Name,
            AgentEditorField::Tools => AgentEditorField::Model,
            AgentEditorField::Model => AgentEditorField::Name,
            AgentEditorField::GenerateDescription => AgentEditorField::GenerateDescription,
        };
        if matches!(
            self.draft.field,
            AgentEditorField::Name
                | AgentEditorField::Description
                | AgentEditorField::Instructions
                | AgentEditorField::GenerateDescription
        ) {
            self.move_draft_cursor_to_current_end();
        }
    }

    pub fn to_agent_draft(&self) -> AgentDraft {
        AgentDraft {
            name: self.draft.name.trim().to_string(),
            description: self.draft.description.trim().to_string(),
            short_description: None,
            instructions: self.draft.instructions.trim().to_string(),
            tools: self.draft.tools.clone(),
            disallow_tools: self.draft.disallow_tools.clone(),
            model: self.draft.model.clone(),
        }
    }

    pub fn sync_model_selection_to_draft(&mut self) {
        let Some(model_value) = self.draft.model.as_deref() else {
            self.model_selected = 0;
            return;
        };
        if let Some(idx) = self.model_entries.iter().position(|entry| {
            matches!(
                entry,
                AgentModelEntry::Model {
                    provider_key,
                    model
                } if format!("{}/{}", provider_key, model.id) == model_value
            )
        }) {
            self.model_selected = idx;
        }
    }
}

impl AgentEditorDraft {
    fn new(source_kind: AgentSourceKind, model: Option<String>) -> Self {
        Self {
            source_kind,
            original_path: None,
            name: String::new(),
            description: String::new(),
            instructions: String::new(),
            tools: Vec::new(),
            disallow_tools: Vec::new(),
            model,
            field: AgentEditorField::Name,
            generated_description: String::new(),
            cursor: 0,
        }
    }
}

fn char_to_byte_idx(text: &str, char_idx: usize) -> usize {
    text.char_indices()
        .map(|(idx, _)| idx)
        .nth(char_idx)
        .unwrap_or(text.len())
}

#[derive(Clone, Copy)]
struct WrappedDraftLine {
    start_char: usize,
    end_char: usize,
}

fn wrapped_text_cursor(text: &str, cursor: usize, width: usize) -> Option<(usize, usize)> {
    let lines = wrapped_text_lines(text, width);
    wrapped_text_cursor_from_lines(text, cursor, &lines)
}

fn wrapped_text_cursor_from_lines(
    text: &str,
    cursor: usize,
    lines: &[WrappedDraftLine],
) -> Option<(usize, usize)> {
    for (line_idx, line) in lines.iter().enumerate() {
        if cursor >= line.start_char && cursor <= line.end_char {
            return Some((line_idx, draft_display_width(text, line.start_char, cursor)));
        }
    }
    lines.last().map(|line| {
        (
            lines.len().saturating_sub(1),
            draft_display_width(text, line.start_char, line.end_char),
        )
    })
}

fn wrapped_text_line_col_to_char(
    text: &str,
    width: usize,
    target_line: usize,
    target_col: usize,
) -> usize {
    let lines = wrapped_text_lines(text, width);
    wrapped_text_line_col_to_char_from_lines(text, target_line, target_col, &lines)
}

fn wrapped_text_line_col_to_char_from_lines(
    text: &str,
    target_line: usize,
    target_col: usize,
    lines: &[WrappedDraftLine],
) -> usize {
    let Some(line) = lines.get(target_line).copied() else {
        return text.chars().count();
    };
    let mut col = 0usize;
    for (idx, ch) in text
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

fn wrapped_text_lines(text: &str, width: usize) -> Vec<WrappedDraftLine> {
    let chars = text.chars().collect::<Vec<_>>();
    if chars.is_empty() {
        return vec![WrappedDraftLine {
            start_char: 0,
            end_char: 0,
        }];
    }

    let width = width.max(1);
    let mut lines = Vec::new();
    let mut start = 0usize;
    let mut line_width = 0usize;
    for (idx, ch) in chars.iter().copied().enumerate() {
        if ch == '\n' {
            lines.push(WrappedDraftLine {
                start_char: start,
                end_char: idx,
            });
            start = idx + 1;
            line_width = 0;
            continue;
        }

        let ch_width = char_display_width(ch);
        if line_width > 0 && line_width + ch_width > width {
            lines.push(WrappedDraftLine {
                start_char: start,
                end_char: idx,
            });
            start = idx;
            line_width = 0;
        }
        line_width += ch_width;
    }
    lines.push(WrappedDraftLine {
        start_char: start,
        end_char: chars.len(),
    });
    lines
}

fn draft_display_width(text: &str, start: usize, end: usize) -> usize {
    text.chars()
        .skip(start)
        .take(end.saturating_sub(start))
        .map(char_display_width)
        .sum()
}

fn char_display_width(ch: char) -> usize {
    UnicodeWidthChar::width(ch).unwrap_or(0).max(1)
}

fn build_agent_model_entries(providers: &HashMap<String, ProviderProfile>) -> Vec<AgentModelEntry> {
    let mut entries = Vec::new();
    entries.push(AgentModelEntry::Inherit);
    let mut sorted: Vec<_> = providers.iter().collect();
    sorted.sort_by(|a, b| a.1.name.cmp(&b.1.name));
    for (provider_key, profile) in sorted {
        entries.push(AgentModelEntry::ProviderHeader {
            name: profile.name.clone(),
        });
        let mut sorted_models: Vec<_> = profile.models.iter().collect();
        sorted_models.sort_by(|a, b| a.id.cmp(&b.id));
        for model in sorted_models {
            entries.push(AgentModelEntry::Model {
                provider_key: provider_key.clone(),
                model: model.clone(),
            });
        }
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::config::ProviderType;

    fn providers() -> HashMap<String, ProviderProfile> {
        HashMap::from([(
            "openai".to_string(),
            ProviderProfile {
                name: "OpenAI".to_string(),
                endpoint: ProviderType::OpenAI,
                base_url: "https://example.invalid".to_string(),
                models: vec![ModelConfig {
                    id: "gpt-test".to_string(),
                    name: Some("GPT Test".to_string()),
                    limit: 1000,
                    thinking: false,
                    input_modalities: None,
                    extra_body: None,
                    extra_headers: None,
                }],
            },
        )])
    }

    fn manager() -> AgentManagerState {
        AgentManagerState::new(
            Vec::new(),
            providers(),
            "openai".to_string(),
            "gpt-test".to_string(),
        )
    }

    #[test]
    fn create_flow_defaults_to_inherited_model() {
        let mut manager = manager();

        manager.start_create();

        assert_eq!(
            manager.view,
            AgentManagerView::Create(AgentCreateStep::Scope)
        );
        assert_eq!(manager.create_scope_selected, 0);
        assert_eq!(manager.create_method_selected, 0);
        assert_eq!(manager.model_selected, 0);
        assert!(manager.draft.model.is_none());
        assert!(matches!(
            manager.model_entries.first(),
            Some(AgentModelEntry::Inherit)
        ));
    }

    #[test]
    fn agent_draft_keeps_run_agent_policy() {
        let mut manager = manager();
        manager.draft.name = "safe".to_string();
        manager.draft.description = "Safe helper".to_string();
        manager.draft.instructions = "Read and summarize.".to_string();
        manager.draft.tools = vec!["read".to_string(), "run_agent".to_string()];

        let draft = manager.to_agent_draft();

        assert_eq!(
            draft.tools,
            vec!["read".to_string(), "run_agent".to_string()]
        );
        assert!(draft.model.is_none());
    }

    #[test]
    fn draft_cursor_inserts_inside_current_field() {
        let mut manager = manager();
        manager.draft.name = "aget".to_string();
        manager.draft.cursor = 1;

        manager.insert_draft_char('n');

        assert_eq!(manager.draft.name, "anget");
        assert_eq!(manager.draft.cursor, 2);
    }

    #[test]
    fn draft_backspace_deletes_before_cursor() {
        let mut manager = manager();
        manager.draft.name = "agent".to_string();
        manager.draft.cursor = 3;

        manager.backspace_draft_char();

        assert_eq!(manager.draft.name, "agnt");
        assert_eq!(manager.draft.cursor, 2);
    }

    #[test]
    fn generation_failure_restores_create_description_input() {
        let mut manager = manager();
        manager.draft.generated_description = "review git diffs".to_string();
        manager.start_generating(AgentGenerateReturn::CreateFlow);

        manager.fail_generation("failed".to_string());

        assert_eq!(
            manager.view,
            AgentManagerView::Create(AgentCreateStep::GenerateDescription)
        );
        assert_eq!(manager.draft.generated_description, "review git diffs");
        assert_eq!(manager.message.as_deref(), Some("failed"));
        assert_eq!(manager.draft.field, AgentEditorField::GenerateDescription);
    }

    #[test]
    fn generated_draft_leaves_generating_for_preview() {
        let mut manager = manager();
        manager.start_generating(AgentGenerateReturn::Direct);

        manager.apply_generated(
            AgentSourceKind::Project,
            AgentDraft {
                name: "git-reviewer".to_string(),
                description: "Reviews git changes.".to_string(),
                short_description: None,
                instructions: "Review the diff.".to_string(),
                tools: vec!["read".to_string()],
                disallow_tools: Vec::new(),
                model: None,
            },
        );

        assert_eq!(manager.view, AgentManagerView::GeneratedPreview);
        assert_eq!(manager.draft.name, "git-reviewer");
        assert!(manager.message.is_none());
    }

    #[test]
    fn editable_record_opens_existing_agent_edit_menu() {
        let mut manager = manager();
        manager.start_edit(AgentRecord {
            name: "reviewer".to_string(),
            description: "Reviews changes.".to_string(),
            short_description: None,
            instructions: "Read diffs.".to_string(),
            tools: vec!["read".to_string()],
            disallow_tools: Vec::new(),
            model: Some("openai/gpt-test".to_string()),
            source_kind: AgentSourceKind::Project,
            path: Some(PathBuf::from("/tmp/reviewer.md")),
            editable: true,
        });

        assert_eq!(manager.view, AgentManagerView::EditMenu);
        assert_eq!(
            manager.draft.original_path,
            Some(PathBuf::from("/tmp/reviewer.md"))
        );
        assert_eq!(manager.draft.name, "reviewer");
        assert_eq!(manager.draft.tools, vec!["read".to_string()]);
        assert_eq!(manager.model_selected, 2);
    }

    #[test]
    fn editor_cycles_text_tools_and_model_fields() {
        let mut manager = manager();
        manager.draft.original_path = Some(PathBuf::from("/tmp/reviewer.md"));
        assert_eq!(manager.draft.field, AgentEditorField::Name);

        manager.cycle_field();
        assert_eq!(manager.draft.field, AgentEditorField::Description);
        manager.cycle_field();
        assert_eq!(manager.draft.field, AgentEditorField::Instructions);
        manager.cycle_field();
        assert_eq!(manager.draft.field, AgentEditorField::Tools);
        manager.cycle_field();
        assert_eq!(manager.draft.field, AgentEditorField::Model);
        manager.cycle_field();
        assert_eq!(manager.draft.field, AgentEditorField::Name);
    }

    #[test]
    fn generated_preview_cycles_only_text_fields() {
        let mut manager = manager();
        manager.apply_generated(
            AgentSourceKind::Project,
            AgentDraft {
                name: "git-reviewer".to_string(),
                description: "Reviews git changes.".to_string(),
                short_description: None,
                instructions: "Review the diff.".to_string(),
                tools: vec!["read".to_string()],
                disallow_tools: Vec::new(),
                model: Some("openai/gpt-test".to_string()),
            },
        );

        assert_eq!(manager.draft.original_path, None);
        assert_eq!(manager.draft.field, AgentEditorField::Name);
        manager.cycle_field();
        assert_eq!(manager.draft.field, AgentEditorField::Description);
        manager.cycle_field();
        assert_eq!(manager.draft.field, AgentEditorField::Instructions);
        manager.cycle_field();
        assert_eq!(manager.draft.field, AgentEditorField::Name);
    }

    #[test]
    fn multiline_draft_cursor_moves_vertically_across_wrapped_lines() {
        let mut manager = manager();
        manager.draft.field = AgentEditorField::Instructions;
        manager.draft.instructions = "abcd efgh ijkl".to_string();
        manager.draft.cursor = manager.draft.instructions.chars().count();
        manager.set_draft_wrap_width(4);

        assert!(manager.move_draft_cursor_up());
        assert_eq!(manager.draft.cursor, 10);

        assert!(manager.move_draft_cursor_down());
        assert_eq!(manager.draft.cursor, 14);
    }

    #[test]
    fn multiline_draft_cursor_moves_across_manual_newlines() {
        let mut manager = manager();
        manager.draft.field = AgentEditorField::Instructions;
        manager.draft.instructions = "alpha\nbeta".to_string();
        manager.draft.cursor = manager.draft.instructions.chars().count();
        manager.set_draft_wrap_width(80);

        assert!(manager.move_draft_cursor_up());
        assert_eq!(manager.draft.cursor, 4);
    }

    #[test]
    fn description_cursor_moves_vertically_across_wrapped_lines() {
        let mut manager = manager();
        manager.draft.field = AgentEditorField::Description;
        manager.draft.description = "abcd efgh ijkl".to_string();
        manager.draft.cursor = manager.draft.description.chars().count();
        manager.set_draft_wrap_width(4);

        assert!(manager.current_field_is_multiline());
        assert!(manager.move_draft_cursor_up());
        assert_eq!(manager.draft.cursor, 10);
    }
}
