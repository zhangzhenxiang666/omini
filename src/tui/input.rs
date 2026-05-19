use super::state::{
    AgentCreateStep, AgentGenerateReturn, AgentManagerView, AgentModelEntry, AgentStatus,
    InteractionStep, ModelSelectionEntry, UiMessage, UiState,
};
use crate::subagents::AgentSourceKind;
use crate::types::events::{ToolPauseKind, ToolPauseResponse, UiToRuntimeEvent};
use crossterm::event::{KeyCode, KeyModifiers};
use tokio::sync::mpsc;

const AGENT_TOOL_ROW_COUNT: usize = 13;
const AGENT_TOOL_NAMES: [&str; 5] = ["read", "bash", "edit", "write", "ask_user"];
const AGENT_EDIT_ACTION_COUNT: usize = 4;

/// 处理交互模式的键盘事件。
/// 返回 `true` = 事件已消费；`false` = 调用方应退出交互模式。
pub(super) async fn handle_interaction_key(
    step: &mut InteractionStep,
    key: KeyCode,
    request_tx: &mpsc::Sender<UiToRuntimeEvent>,
) -> bool {
    match step {
        InteractionStep::ModelSelection {
            entries,
            selected,
            thinking_idx,
            ..
        } => {
            use ModelSelectionEntry as E;
            match key {
                KeyCode::Up => {
                    let mut new = selected.saturating_sub(1);
                    while new > 0 && matches!(&entries[new], E::ProviderHeader { .. }) {
                        new = new.saturating_sub(1);
                    }
                    if !matches!(&entries[new], E::ProviderHeader { .. }) {
                        *selected = new;
                    }
                    true
                }
                KeyCode::Down => {
                    let max = entries.len().saturating_sub(1);
                    let mut new = (*selected + 1).min(max);
                    while new < max && matches!(&entries[new], E::ProviderHeader { .. }) {
                        new = (new + 1).min(max);
                    }
                    if !matches!(&entries[new], E::ProviderHeader { .. }) {
                        *selected = new;
                    }
                    true
                }
                KeyCode::Left => {
                    if let E::Model { model, .. } = &entries[*selected]
                        && model.thinking
                    {
                        *thinking_idx = thinking_idx.saturating_sub(1);
                    }
                    true
                }
                KeyCode::Right => {
                    if let E::Model { model, .. } = &entries[*selected]
                        && model.thinking
                    {
                        *thinking_idx = (*thinking_idx + 1).min(3);
                    }
                    true
                }
                KeyCode::Enter => {
                    if let E::Model {
                        provider_key,
                        model,
                    } = &entries[*selected]
                    {
                        let thinking_effort = match *thinking_idx {
                            1 => Some(crate::types::config::ThinkingEffort::Low),
                            2 => Some(crate::types::config::ThinkingEffort::Medium),
                            3 => Some(crate::types::config::ThinkingEffort::High),
                            _ => None,
                        };
                        let _ = request_tx
                            .send(UiToRuntimeEvent::ModelSelected {
                                provider: provider_key.clone(),
                                model: model.id.clone(),
                                thinking_effort,
                            })
                            .await;
                    }
                    true
                }
                KeyCode::Esc => false,
                _ => true,
            }
        }
        InteractionStep::Session {
            sessions,
            all_sessions,
            search,
            selected,
        } => match key {
            KeyCode::Up => {
                *selected = selected.saturating_sub(1);
                true
            }
            KeyCode::Down => {
                *selected = (*selected + 1).min(sessions.len().saturating_sub(1));
                true
            }
            KeyCode::Enter => {
                if !sessions.is_empty() {
                    let session_id = sessions[*selected].id.clone();
                    let _ = request_tx
                        .send(UiToRuntimeEvent::SessionSelected { session_id })
                        .await;
                }
                true
            }
            KeyCode::Char(c) => {
                search.push(c);
                let lower = search.to_lowercase();
                let mut filtered: Vec<_> = all_sessions
                    .iter()
                    .filter(|s| s.title.to_lowercase().contains(&lower))
                    .cloned()
                    .collect();
                std::mem::swap(sessions, &mut filtered);
                *selected = 0;
                true
            }
            KeyCode::Backspace => {
                search.pop();
                let lower = search.to_lowercase();
                if lower.is_empty() {
                    *sessions = all_sessions.clone();
                } else {
                    let mut filtered: Vec<_> = all_sessions
                        .iter()
                        .filter(|s| s.title.to_lowercase().contains(&lower))
                        .cloned()
                        .collect();
                    std::mem::swap(sessions, &mut filtered);
                }
                *selected = 0;
                true
            }
            KeyCode::Esc => false,
            _ => true,
        },
        InteractionStep::Agents(manager) => {
            if matches!(manager.view, AgentManagerView::Generating(_)) {
                return true;
            }
            match key {
                KeyCode::Esc => {
                    match manager.view {
                        AgentManagerView::List => return false,
                        AgentManagerView::Create(step) => {
                            step_agent_create_back(manager, step);
                        }
                        AgentManagerView::ConfirmDelete(idx) => {
                            manager.view = AgentManagerView::Detail(idx);
                        }
                        AgentManagerView::EditMetadata
                        | AgentManagerView::EditTools
                        | AgentManagerView::EditModel => {
                            autosave_agent_edit(manager, request_tx).await;
                            manager.view = AgentManagerView::EditMenu;
                        }
                        AgentManagerView::EditMenu => {
                            manager.view = AgentManagerView::List;
                        }
                        _ => manager.view = AgentManagerView::List,
                    }
                    true
                }
                KeyCode::Up => {
                    if manager.view == AgentManagerView::EditTools
                        && manager.draft.field == super::state::AgentEditorField::Tools
                    {
                        manager.tool_selected = manager.tool_selected.saturating_sub(1);
                    } else if manager.view == AgentManagerView::EditModel
                        && manager.draft.field == super::state::AgentEditorField::Model
                    {
                        select_prev_agent_model(manager);
                        apply_selected_agent_model(manager);
                    } else if agent_text_input_active(manager)
                        && manager.current_field_is_multiline()
                    {
                        manager.move_draft_cursor_up();
                    } else {
                        match manager.view {
                            AgentManagerView::List => {
                                manager.selected = manager.selected.saturating_sub(1);
                            }
                            AgentManagerView::Detail(idx) => {
                                let max = detail_action_count(manager, idx).saturating_sub(1);
                                manager.detail_action_selected =
                                    manager.detail_action_selected.saturating_sub(1).min(max);
                            }
                            AgentManagerView::EditMenu => {
                                manager.edit_action_selected =
                                    manager.edit_action_selected.saturating_sub(1);
                            }
                            AgentManagerView::Create(AgentCreateStep::Scope) => {
                                manager.create_scope_selected =
                                    manager.create_scope_selected.saturating_sub(1);
                            }
                            AgentManagerView::Create(AgentCreateStep::Tools) => {
                                manager.tool_selected = manager.tool_selected.saturating_sub(1);
                            }
                            AgentManagerView::Create(AgentCreateStep::Model) => {
                                select_prev_agent_model(manager);
                            }
                            AgentManagerView::Create(AgentCreateStep::Method) => {
                                manager.create_method_selected =
                                    manager.create_method_selected.saturating_sub(1);
                            }
                            _ => {}
                        }
                    }
                    true
                }
                KeyCode::Down => {
                    if manager.view == AgentManagerView::EditTools
                        && manager.draft.field == super::state::AgentEditorField::Tools
                    {
                        manager.tool_selected =
                            (manager.tool_selected + 1).min(AGENT_TOOL_ROW_COUNT - 1);
                    } else if manager.view == AgentManagerView::EditModel
                        && manager.draft.field == super::state::AgentEditorField::Model
                    {
                        select_next_agent_model(manager);
                        apply_selected_agent_model(manager);
                    } else if agent_text_input_active(manager)
                        && manager.current_field_is_multiline()
                    {
                        manager.move_draft_cursor_down();
                    } else {
                        match manager.view {
                            AgentManagerView::List => {
                                manager.selected =
                                    (manager.selected + 1).min(manager.records.len());
                            }
                            AgentManagerView::Detail(idx) => {
                                let max = detail_action_count(manager, idx).saturating_sub(1);
                                manager.detail_action_selected =
                                    (manager.detail_action_selected + 1).min(max);
                            }
                            AgentManagerView::EditMenu => {
                                manager.edit_action_selected = (manager.edit_action_selected + 1)
                                    .min(AGENT_EDIT_ACTION_COUNT - 1);
                            }
                            AgentManagerView::Create(AgentCreateStep::Scope) => {
                                manager.create_scope_selected =
                                    (manager.create_scope_selected + 1).min(1);
                            }
                            AgentManagerView::Create(AgentCreateStep::Tools) => {
                                manager.tool_selected =
                                    (manager.tool_selected + 1).min(AGENT_TOOL_ROW_COUNT - 1);
                            }
                            AgentManagerView::Create(AgentCreateStep::Model) => {
                                select_next_agent_model(manager);
                            }
                            AgentManagerView::Create(AgentCreateStep::Method) => {
                                manager.create_method_selected =
                                    (manager.create_method_selected + 1).min(1);
                            }
                            _ => {}
                        }
                    }
                    true
                }
                KeyCode::Left => {
                    if agent_text_input_active(manager) {
                        manager.move_draft_cursor_left();
                    }
                    true
                }
                KeyCode::Right => {
                    if agent_text_input_active(manager) {
                        manager.move_draft_cursor_right();
                    }
                    true
                }
                KeyCode::Char('c') if manager.view == AgentManagerView::List => {
                    manager.start_create();
                    true
                }
                KeyCode::Enter => {
                    handle_agents_enter(manager, request_tx).await;
                    true
                }
                KeyCode::Tab => {
                    match manager.view {
                        AgentManagerView::GeneratedPreview => {
                            manager.cycle_field();
                        }
                        AgentManagerView::EditMetadata => {
                            manager.draft.field = match manager.draft.field {
                                super::state::AgentEditorField::Name => {
                                    super::state::AgentEditorField::Description
                                }
                                super::state::AgentEditorField::Description => {
                                    super::state::AgentEditorField::Instructions
                                }
                                _ => super::state::AgentEditorField::Name,
                            };
                            manager.move_draft_cursor_to_current_end();
                        }
                        _ => {}
                    }
                    true
                }
                KeyCode::Backspace => {
                    match manager.view {
                        AgentManagerView::Generate
                        | AgentManagerView::EditMetadata
                        | AgentManagerView::Create(AgentCreateStep::ManualName)
                        | AgentManagerView::Create(AgentCreateStep::ManualDescription)
                        | AgentManagerView::Create(AgentCreateStep::ManualInstructions)
                        | AgentManagerView::Create(AgentCreateStep::GenerateDescription) => {
                            manager.backspace_draft_char();
                        }
                        AgentManagerView::GeneratedPreview if agent_text_input_active(manager) => {
                            manager.backspace_draft_char();
                        }
                        _ => {}
                    }
                    true
                }
                KeyCode::Char(' ')
                    if manager.view == AgentManagerView::Create(AgentCreateStep::Tools) =>
                {
                    toggle_agent_tool_group_or_item(manager);
                    true
                }
                KeyCode::Char(' ')
                    if manager.view == AgentManagerView::GeneratedPreview
                        && manager.draft.field == super::state::AgentEditorField::Tools =>
                {
                    toggle_agent_tool_group_or_item(manager);
                    true
                }
                KeyCode::Char(' ') if manager.view == AgentManagerView::EditTools => {
                    toggle_agent_tool_group_or_item(manager);
                    true
                }
                KeyCode::Char(c) if agent_text_input_active(manager) => {
                    manager.insert_draft_char(c);
                    true
                }
                _ => true,
            }
        }
    }
}

async fn handle_agents_enter(
    manager: &mut super::state::AgentManagerState,
    request_tx: &mpsc::Sender<UiToRuntimeEvent>,
) {
    match manager.view.clone() {
        AgentManagerView::List => {
            if manager.selected == 0 {
                manager.start_create();
            } else {
                manager.detail_action_selected = 0;
                manager.view = AgentManagerView::Detail(manager.selected - 1);
            }
        }
        AgentManagerView::Detail(idx) => {
            handle_agent_detail_action(manager, idx);
        }
        AgentManagerView::EditMenu => {
            handle_agent_edit_menu_enter(manager);
        }
        AgentManagerView::EditMetadata => {
            autosave_agent_edit(manager, request_tx).await;
            manager.view = AgentManagerView::EditMenu;
        }
        AgentManagerView::EditTools | AgentManagerView::EditModel => {
            autosave_agent_edit(manager, request_tx).await;
            manager.view = AgentManagerView::EditMenu;
        }
        AgentManagerView::GeneratedPreview => {
            let draft = manager.to_agent_draft();
            let _ = request_tx
                .send(UiToRuntimeEvent::AgentSaveRequested {
                    source_kind: manager.draft.source_kind,
                    original_path: manager.draft.original_path.clone(),
                    draft,
                })
                .await;
        }
        AgentManagerView::Generate => {
            submit_agent_generate(manager, request_tx, AgentGenerateReturn::Direct).await;
        }
        AgentManagerView::Generating(_) => {}
        AgentManagerView::Create(step) => {
            handle_agent_create_enter(manager, request_tx, step).await;
        }
        AgentManagerView::ConfirmDelete(idx) => {
            if let Some(path) = manager
                .records
                .get(idx)
                .and_then(|record| record.path.clone())
            {
                let _ = request_tx
                    .send(UiToRuntimeEvent::AgentDeleteRequested { path })
                    .await;
            }
        }
    }
}

fn handle_agent_edit_menu_enter(manager: &mut super::state::AgentManagerState) {
    match manager
        .edit_action_selected
        .min(AGENT_EDIT_ACTION_COUNT - 1)
    {
        0 => {
            manager.draft.field = super::state::AgentEditorField::Name;
            manager.move_draft_cursor_to_current_end();
            manager.view = AgentManagerView::EditMetadata;
        }
        1 => {
            manager.draft.field = super::state::AgentEditorField::Tools;
            manager.view = AgentManagerView::EditTools;
        }
        2 => {
            manager.draft.field = super::state::AgentEditorField::Model;
            manager.sync_model_selection_to_draft();
            manager.view = AgentManagerView::EditModel;
        }
        _ => manager.view = AgentManagerView::List,
    }
}

async fn autosave_agent_edit(
    manager: &super::state::AgentManagerState,
    request_tx: &mpsc::Sender<UiToRuntimeEvent>,
) {
    let draft = manager.to_agent_draft();
    let _ = request_tx
        .send(UiToRuntimeEvent::AgentSaveRequested {
            source_kind: manager.draft.source_kind,
            original_path: manager.draft.original_path.clone(),
            draft,
        })
        .await;
}

async fn handle_agent_create_enter(
    manager: &mut super::state::AgentManagerState,
    request_tx: &mpsc::Sender<UiToRuntimeEvent>,
    step: AgentCreateStep,
) {
    match step {
        AgentCreateStep::Scope => {
            manager.draft.source_kind = if manager.create_scope_selected == 0 {
                AgentSourceKind::Project
            } else {
                AgentSourceKind::User
            };
            manager.view = AgentManagerView::Create(AgentCreateStep::Tools);
        }
        AgentCreateStep::Tools => {
            manager.view = AgentManagerView::Create(AgentCreateStep::Model);
        }
        AgentCreateStep::Model => {
            apply_selected_agent_model(manager);
            manager.view = AgentManagerView::Create(AgentCreateStep::Method);
        }
        AgentCreateStep::Method => {
            if manager.create_method_selected == 0 {
                manager.draft.field = super::state::AgentEditorField::GenerateDescription;
                manager.move_draft_cursor_to_current_end();
                manager.view = AgentManagerView::Create(AgentCreateStep::GenerateDescription);
            } else {
                manager.draft.field = super::state::AgentEditorField::Name;
                manager.move_draft_cursor_to_current_end();
                manager.view = AgentManagerView::Create(AgentCreateStep::ManualName);
            }
        }
        AgentCreateStep::ManualName => {
            manager.draft.field = super::state::AgentEditorField::Description;
            manager.move_draft_cursor_to_current_end();
            manager.view = AgentManagerView::Create(AgentCreateStep::ManualDescription);
        }
        AgentCreateStep::ManualDescription => {
            manager.draft.field = super::state::AgentEditorField::Instructions;
            manager.move_draft_cursor_to_current_end();
            manager.view = AgentManagerView::Create(AgentCreateStep::ManualInstructions);
        }
        AgentCreateStep::ManualInstructions => {
            let draft = manager.to_agent_draft();
            let _ = request_tx
                .send(UiToRuntimeEvent::AgentSaveRequested {
                    source_kind: manager.draft.source_kind,
                    original_path: None,
                    draft,
                })
                .await;
        }
        AgentCreateStep::GenerateDescription => {
            submit_agent_generate(manager, request_tx, AgentGenerateReturn::CreateFlow).await;
        }
    }
}

async fn submit_agent_generate(
    manager: &mut super::state::AgentManagerState,
    request_tx: &mpsc::Sender<UiToRuntimeEvent>,
    return_to: AgentGenerateReturn,
) {
    let description = manager.draft.generated_description.trim().to_string();
    if description.is_empty() {
        manager.message = Some("请先填写用途描述，再生成 agent".to_string());
        manager.draft.field = super::state::AgentEditorField::GenerateDescription;
        manager.move_draft_cursor_to_current_end();
        return;
    }

    let sent = request_tx
        .send(UiToRuntimeEvent::AgentGenerateRequested {
            source_kind: manager.draft.source_kind,
            description,
            tools: manager.draft.tools.clone(),
            disallow_tools: manager.draft.disallow_tools.clone(),
            model: manager.draft.model.clone(),
        })
        .await;
    if sent.is_ok() {
        manager.start_generating(return_to);
    }
}

fn step_agent_create_back(manager: &mut super::state::AgentManagerState, step: AgentCreateStep) {
    match step {
        AgentCreateStep::Scope => manager.view = AgentManagerView::List,
        AgentCreateStep::Tools => manager.view = AgentManagerView::Create(AgentCreateStep::Scope),
        AgentCreateStep::Model => manager.view = AgentManagerView::Create(AgentCreateStep::Tools),
        AgentCreateStep::Method => manager.view = AgentManagerView::Create(AgentCreateStep::Model),
        AgentCreateStep::ManualName | AgentCreateStep::GenerateDescription => {
            manager.view = AgentManagerView::Create(AgentCreateStep::Method);
        }
        AgentCreateStep::ManualDescription => {
            manager.draft.field = super::state::AgentEditorField::Name;
            manager.move_draft_cursor_to_current_end();
            manager.view = AgentManagerView::Create(AgentCreateStep::ManualName);
        }
        AgentCreateStep::ManualInstructions => {
            manager.draft.field = super::state::AgentEditorField::Description;
            manager.move_draft_cursor_to_current_end();
            manager.view = AgentManagerView::Create(AgentCreateStep::ManualDescription);
        }
    }
}

fn agent_text_input_active(manager: &super::state::AgentManagerState) -> bool {
    match manager.view {
        AgentManagerView::GeneratedPreview => matches!(
            manager.draft.field,
            super::state::AgentEditorField::Name
                | super::state::AgentEditorField::Description
                | super::state::AgentEditorField::Instructions
        ),
        AgentManagerView::EditMetadata => matches!(
            manager.draft.field,
            super::state::AgentEditorField::Name
                | super::state::AgentEditorField::Description
                | super::state::AgentEditorField::Instructions
        ),
        AgentManagerView::Generate
        | AgentManagerView::Create(AgentCreateStep::ManualName)
        | AgentManagerView::Create(AgentCreateStep::ManualDescription)
        | AgentManagerView::Create(AgentCreateStep::ManualInstructions)
        | AgentManagerView::Create(AgentCreateStep::GenerateDescription) => true,
        _ => false,
    }
}

fn apply_selected_agent_model(manager: &mut super::state::AgentManagerState) {
    match manager.model_entries.get(manager.model_selected) {
        Some(AgentModelEntry::Inherit) => manager.draft.model = None,
        Some(AgentModelEntry::Model {
            provider_key,
            model,
        }) => manager.draft.model = Some(format!("{}/{}", provider_key, model.id)),
        Some(AgentModelEntry::ProviderHeader { .. }) | None => {}
    }
}

fn detail_action_count(manager: &super::state::AgentManagerState, idx: usize) -> usize {
    manager
        .records
        .get(idx)
        .map(|record| if record.editable { 3 } else { 1 })
        .unwrap_or(0)
}

fn handle_agent_detail_action(manager: &mut super::state::AgentManagerState, idx: usize) {
    let Some(record) = manager.records.get(idx).cloned() else {
        manager.view = AgentManagerView::List;
        return;
    };
    let selected = manager
        .detail_action_selected
        .min(detail_action_count(manager, idx).saturating_sub(1));
    if record.editable {
        match selected {
            0 => manager.start_edit(record),
            1 => manager.view = AgentManagerView::ConfirmDelete(idx),
            _ => manager.view = AgentManagerView::List,
        }
    } else {
        manager.view = AgentManagerView::List;
    }
}

fn select_prev_agent_model(manager: &mut super::state::AgentManagerState) {
    let mut new = manager.model_selected.saturating_sub(1);
    while new > 0
        && matches!(
            manager.model_entries.get(new),
            Some(AgentModelEntry::ProviderHeader { .. })
        )
    {
        new = new.saturating_sub(1);
    }
    if matches!(
        manager.model_entries.get(new),
        Some(AgentModelEntry::Model { .. }) | Some(AgentModelEntry::Inherit)
    ) {
        manager.model_selected = new;
    }
}

fn select_next_agent_model(manager: &mut super::state::AgentManagerState) {
    let max = manager.model_entries.len().saturating_sub(1);
    let mut new = (manager.model_selected + 1).min(max);
    while new < max
        && matches!(
            manager.model_entries.get(new),
            Some(AgentModelEntry::ProviderHeader { .. })
        )
    {
        new = (new + 1).min(max);
    }
    if matches!(
        manager.model_entries.get(new),
        Some(AgentModelEntry::Model { .. }) | Some(AgentModelEntry::Inherit)
    ) {
        manager.model_selected = new;
    }
}

fn toggle_agent_tool_group_or_item(manager: &mut super::state::AgentManagerState) {
    match manager.tool_selected {
        0 => {
            manager.draft.tools.clear();
            manager.draft.disallow_tools.clear();
        }
        1 => toggle_allow_group(manager, &["read"]),
        2 => toggle_allow_group(manager, &["bash", "edit", "write"]),
        3..=7 => {
            let tool = AGENT_TOOL_NAMES[manager.tool_selected - 3];
            if manager.draft.tools.is_empty() {
                toggle_tool(&mut manager.draft.disallow_tools, tool);
            } else {
                toggle_tool(&mut manager.draft.tools, tool);
                manager.draft.disallow_tools.retain(|item| item != tool);
            }
        }
        8..=12 => {
            let tool = AGENT_TOOL_NAMES[manager.tool_selected - 8];
            if manager.draft.disallow_tools.iter().any(|item| item == tool) {
                manager.draft.disallow_tools.retain(|item| item != tool);
            } else {
                manager.draft.disallow_tools.push(tool.to_string());
                manager.draft.tools.retain(|item| item != tool);
            }
        }
        _ => {}
    }
    manager.draft.tools.retain(|tool| tool != "subagent");
    manager.draft.tools.sort();
    manager.draft.tools.dedup();
    manager
        .draft
        .disallow_tools
        .retain(|tool| tool != "subagent");
    manager.draft.disallow_tools.sort();
    manager.draft.disallow_tools.dedup();
}

fn toggle_allow_group(manager: &mut super::state::AgentManagerState, group: &[&str]) {
    if manager.draft.tools.is_empty() {
        let enabled = group
            .iter()
            .all(|tool| !manager.draft.disallow_tools.iter().any(|item| item == tool));
        if enabled {
            for tool in group {
                add_tool(&mut manager.draft.disallow_tools, tool);
            }
        } else {
            manager
                .draft
                .disallow_tools
                .retain(|tool| !group.iter().any(|item| item == &tool.as_str()));
        }
        return;
    }

    let enabled = group
        .iter()
        .all(|tool| manager.draft.tools.iter().any(|item| item == tool));
    if enabled {
        manager
            .draft
            .tools
            .retain(|tool| !group.iter().any(|item| item == &tool.as_str()));
    } else {
        for tool in group {
            add_tool(&mut manager.draft.tools, tool);
        }
    }
    manager
        .draft
        .disallow_tools
        .retain(|tool| !group.iter().any(|item| item == &tool.as_str()));
}

fn toggle_tool(tools: &mut Vec<String>, tool: &str) {
    if tools.iter().any(|item| item == tool) {
        tools.retain(|item| item != tool);
    } else {
        add_tool(tools, tool);
    }
}

fn add_tool(tools: &mut Vec<String>, tool: &str) {
    if !tools.iter().any(|item| item == tool) {
        tools.push(tool.to_string());
    }
}

pub(super) async fn resolve_active_tool_pause(
    state: &mut UiState,
    request_tx: &mpsc::Sender<UiToRuntimeEvent>,
) {
    let Some(req) = state.active_tool_pause().cloned() else {
        return;
    };

    let response = match &req.kind {
        ToolPauseKind::Permission(_) => ToolPauseResponse::Permission {
            approved: state.permission_selected == 0,
        },
        ToolPauseKind::UserInput(preview) => {
            let mut answers = serde_json::Map::new();

            for (idx, question) in preview.questions.iter().enumerate() {
                let custom_idx = question.options.len();
                let selected = state
                    .user_input_selected
                    .get(idx)
                    .copied()
                    .unwrap_or(0)
                    .min(custom_idx);
                let label = if selected == custom_idx {
                    "None of the above".to_string()
                } else {
                    question.options[selected].label.clone()
                };
                let note = state
                    .user_input_notes
                    .get(idx)
                    .map(|note| note.trim())
                    .filter(|note| !note.is_empty());

                answers.insert(
                    question.id.clone(),
                    serde_json::json!({
                        "label": label,
                        "note": note,
                    }),
                );
            }

            ToolPauseResponse::UserInput {
                value: serde_json::json!({
                    "answers": answers,
                }),
            }
        }
    };

    let _ = request_tx
        .send(UiToRuntimeEvent::ResolveToolPause {
            tool_use_id: req.tool_use_id.clone(),
            response,
        })
        .await;
    state.pending_tool_previews.remove(&req.tool_use_id);
    if state.pending_tool_previews.is_empty() {
        state.reset_permission_drawer();
    }
}

pub(super) async fn flush_queued_user_inputs(
    state: &mut UiState,
    request_tx: &mpsc::Sender<UiToRuntimeEvent>,
) {
    let ui_messages = state
        .queued_user_inputs
        .iter()
        .cloned()
        .map(|draft| match draft.clone().history_item() {
            crate::types::display::HistoryItem::Message(message) => UiMessage::Message(message),
            crate::types::display::HistoryItem::Display(display) => UiMessage::Display(display),
        })
        .collect::<Vec<_>>();
    let Some(draft) = state.take_queued_user_draft() else {
        return;
    };

    state.messages.extend(ui_messages);
    state.scroll_offset = 0;
    state.auto_scroll = true;
    state.agent_status = AgentStatus::Working;
    let _ = request_tx.send(UiToRuntimeEvent::SendMessage(draft)).await;
}

pub(super) async fn submit_queued_intervention(
    state: &mut UiState,
    request_tx: &mpsc::Sender<UiToRuntimeEvent>,
) {
    if state.is_run_active()
        && state.pending_intervention_inputs.is_empty()
        && let Some(draft) = state.take_queued_user_draft_for_intervention()
    {
        let _ = request_tx
            .send(UiToRuntimeEvent::InterveneMessage(draft))
            .await;
    }
}

pub(super) fn is_intervention_key(code: KeyCode, modifiers: KeyModifiers) -> bool {
    modifiers.contains(KeyModifiers::ALT) && matches!(code, KeyCode::Enter)
}

pub(super) fn is_newline_key(code: KeyCode, modifiers: KeyModifiers) -> bool {
    matches!(code, KeyCode::Enter) && modifiers.contains(KeyModifiers::SHIFT)
        || matches!(code, KeyCode::Char('\n'))
        || matches!(code, KeyCode::Char('j')) && modifiers == KeyModifiers::CONTROL
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newline_key_accepts_shift_enter_and_ctrl_j_forms() {
        assert!(is_newline_key(KeyCode::Enter, KeyModifiers::SHIFT));
        assert!(is_newline_key(KeyCode::Char('\n'), KeyModifiers::empty()));
        assert!(is_newline_key(KeyCode::Char('j'), KeyModifiers::CONTROL));
    }

    #[test]
    fn newline_key_rejects_plain_enter_and_intervention_key() {
        assert!(!is_newline_key(KeyCode::Enter, KeyModifiers::empty()));
        assert!(!is_newline_key(KeyCode::Enter, KeyModifiers::ALT));
    }
}
