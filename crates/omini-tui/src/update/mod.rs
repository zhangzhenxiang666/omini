use super::clipboard::copy_to_clipboard;
use super::input;
use super::selection::{
    selected_text, selection_point_from_mouse, update_text_selection_from_mouse,
};
use super::state::{AgentStatus, TextSelection, UiMessage, UiState};
use crate::types::events::{
    PlanApprovalAction, RuntimeToUiEvent, ToolPauseKind, ToolPauseResponse, UiToRuntimeEvent,
};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use tokio::sync::mpsc;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct UpdateOutcome {
    pub redraw: bool,
    pub exit: bool,
}

impl UpdateOutcome {
    fn redraw() -> Self {
        Self {
            redraw: true,
            exit: false,
        }
    }

    fn exit() -> Self {
        Self {
            redraw: false,
            exit: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::events::{
        PermissionPreview, PlanApprovalAction, SubmittedPlan, ToolPauseRequest,
    };
    use chrono::Utc;
    use std::path::PathBuf;

    fn permission_pause(tool_use_id: &str) -> ToolPauseRequest {
        ToolPauseRequest {
            tool_use_id: tool_use_id.to_string(),
            preview_tool_use_id: None,
            tool_name: "bash".to_string(),
            permission_source: None,
            source_session_id: None,
            source_agent_label: None,
            kind: ToolPauseKind::Permission(PermissionPreview::Custom {
                tool_name: "bash".to_string(),
                payload: serde_json::Map::new(),
            }),
        }
    }

    fn state_with_permission_pause() -> UiState {
        let mut state = UiState::new();
        state.apply_event(RuntimeToUiEvent::ToolPauseRequested(permission_pause(
            "tool_1",
        )));
        state
    }

    fn submitted_plan() -> SubmittedPlan {
        SubmittedPlan {
            id: "20260521T000000Z-plan".to_string(),
            title: "Plan".to_string(),
            markdown: "# Plan\n\n- Step".to_string(),
            path: PathBuf::from("/tmp/plan.md"),
            created_at: Utc::now(),
        }
    }

    async fn recv_pause_response(rx: &mut mpsc::Receiver<UiToRuntimeEvent>) -> ToolPauseResponse {
        let Some(UiToRuntimeEvent::ResolveToolPause { response, .. }) = rx.recv().await else {
            panic!("expected tool pause response");
        };
        response
    }

    #[tokio::test]
    async fn tab_starts_permission_deny_note_mode() {
        let mut state = state_with_permission_pause();
        let (tx, _rx) = mpsc::channel(1);

        handle_tool_pause_key(&mut state, KeyCode::Tab, &tx).await;

        assert!(state.user_input_note_mode);
        assert_eq!(state.permission_selected, 1);
        assert_eq!(state.current_user_input_note(), "");
    }

    #[tokio::test]
    async fn permission_deny_note_is_sent_on_enter() {
        let mut state = state_with_permission_pause();
        let (tx, mut rx) = mpsc::channel(1);

        handle_tool_pause_key(&mut state, KeyCode::Tab, &tx).await;
        for c in "Need context".chars() {
            handle_tool_pause_key(&mut state, KeyCode::Char(c), &tx).await;
        }
        handle_tool_pause_key(&mut state, KeyCode::Enter, &tx).await;

        assert_eq!(
            recv_pause_response(&mut rx).await,
            ToolPauseResponse::Permission {
                approved: false,
                note: Some("Need context".to_string()),
            }
        );
    }

    #[tokio::test]
    async fn permission_esc_denies_without_note() {
        let mut state = state_with_permission_pause();
        let (tx, mut rx) = mpsc::channel(1);

        handle_tool_pause_key(&mut state, KeyCode::Esc, &tx).await;

        assert_eq!(
            recv_pause_response(&mut rx).await,
            ToolPauseResponse::Permission {
                approved: false,
                note: None,
            }
        );
    }

    #[tokio::test]
    async fn permission_approval_ignores_stale_note() {
        let mut state = state_with_permission_pause();
        let (tx, mut rx) = mpsc::channel(1);

        handle_tool_pause_key(&mut state, KeyCode::Tab, &tx).await;
        for c in "Do not run".chars() {
            handle_tool_pause_key(&mut state, KeyCode::Char(c), &tx).await;
        }
        handle_tool_pause_key(&mut state, KeyCode::Tab, &tx).await;
        handle_tool_pause_key(&mut state, KeyCode::Char('y'), &tx).await;

        assert_eq!(
            recv_pause_response(&mut rx).await,
            ToolPauseResponse::Permission {
                approved: true,
                note: None,
            }
        );
    }

    #[tokio::test]
    async fn plan_approval_enter_sends_selected_action() {
        let mut state = UiState::new();
        state.apply_event(RuntimeToUiEvent::PlanSubmitted(submitted_plan()));
        state.plan_approval_selected = 1;
        let (tx, mut rx) = mpsc::channel(1);

        handle_plan_approval_key(&mut state, KeyCode::Enter, &tx).await;

        let Some(UiToRuntimeEvent::ResolvePlanApproval { plan_id, action }) = rx.recv().await
        else {
            panic!("expected plan approval response");
        };
        assert_eq!(plan_id, "20260521T000000Z-plan");
        assert_eq!(action, PlanApprovalAction::ApproveAndCompact);
        assert!(state.plan_approval.is_none());
    }

    #[tokio::test]
    async fn plan_approval_esc_continues_discussion() {
        let mut state = UiState::new();
        state.apply_event(RuntimeToUiEvent::PlanSubmitted(submitted_plan()));
        let (tx, mut rx) = mpsc::channel(1);

        handle_plan_approval_key(&mut state, KeyCode::Esc, &tx).await;

        let Some(UiToRuntimeEvent::ResolvePlanApproval { action, .. }) = rx.recv().await else {
            panic!("expected plan approval response");
        };
        assert_eq!(action, PlanApprovalAction::ContinueDiscussing);
        assert!(state.plan_approval.is_none());
    }

    #[tokio::test]
    async fn shift_tab_sends_profile_toggle() {
        let mut state = UiState::new();
        let (tx, mut rx) = mpsc::channel(1);

        let handled =
            handle_key_event(&mut state, KeyCode::BackTab, KeyModifiers::SHIFT, &tx).await;

        assert!(handled);
        let Some(UiToRuntimeEvent::ToggleActiveProfile) = rx.recv().await else {
            panic!("expected profile toggle event");
        };
    }
}

pub(super) async fn handle_input_event(
    state: &mut UiState,
    event: Event,
    request_tx: &mpsc::Sender<UiToRuntimeEvent>,
) -> UpdateOutcome {
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press => {
            if handle_key_event(state, key.code, key.modifiers, request_tx).await {
                UpdateOutcome::redraw()
            } else {
                UpdateOutcome::exit()
            }
        }
        Event::Paste(text)
            if state.interaction_step.is_none()
                && state.active_tool_pause().is_none()
                && state.plan_approval.is_none()
                && state.help_drawer.is_none() =>
        {
            state.insert_paste(text);
            state.update_input_autocomplete();
            UpdateOutcome::redraw()
        }
        Event::Resize(_, _) => UpdateOutcome::redraw(),
        Event::Mouse(mouse) => {
            handle_mouse_event(state, mouse.kind, mouse.row, mouse.column);
            UpdateOutcome::redraw()
        }
        _ => UpdateOutcome::default(),
    }
}

pub(super) async fn handle_runtime_event(
    state: &mut UiState,
    event: RuntimeToUiEvent,
    request_tx: &mpsc::Sender<UiToRuntimeEvent>,
) -> UpdateOutcome {
    if let RuntimeToUiEvent::InteractionRequest(ref req) = event {
        state.open_interaction_request(req);
    }

    if matches!(event, RuntimeToUiEvent::Shutdown) {
        return UpdateOutcome::exit();
    }

    if let RuntimeToUiEvent::SessionChanged {
        session_id,
        messages,
        subagents,
    } = event
    {
        state.apply_session_changed(session_id, messages, subagents);
    } else {
        let should_flush_queue = matches!(event, RuntimeToUiEvent::RunFinished);
        state.apply_event(event);
        if should_flush_queue {
            input::flush_queued_user_inputs(state, request_tx).await;
        }
    }

    UpdateOutcome::redraw()
}

pub(super) async fn drain_runtime_events(
    state: &mut UiState,
    agent_rx: &mut mpsc::Receiver<RuntimeToUiEvent>,
    request_tx: &mpsc::Sender<UiToRuntimeEvent>,
) -> UpdateOutcome {
    let mut outcome = UpdateOutcome::default();
    while let Ok(event) = agent_rx.try_recv() {
        let event_outcome = handle_runtime_event(state, event, request_tx).await;
        outcome.redraw |= event_outcome.redraw;
        if event_outcome.exit {
            outcome.exit = true;
            break;
        }
    }
    outcome
}

async fn handle_key_event(
    state: &mut UiState,
    code: KeyCode,
    modifiers: KeyModifiers,
    request_tx: &mpsc::Sender<UiToRuntimeEvent>,
) -> bool {
    if let Some(ref mut step) = state.interaction_step {
        let consumed = input::handle_interaction_key(step, code, request_tx).await;
        if !consumed {
            state.interaction_step = None;
            state.interaction_request = None;
        }
        return true;
    }

    if state.help_drawer.is_some() {
        handle_help_drawer_key(state, code, modifiers);
        return true;
    }

    if state.plan_approval.is_some() {
        handle_plan_approval_key(state, code, request_tx).await;
        return true;
    }

    if code == KeyCode::Esc
        && matches!(
            state.agent_status,
            AgentStatus::Working | AgentStatus::Thinking
        )
    {
        let _ = request_tx.send(UiToRuntimeEvent::CancelRun).await;
        return true;
    }

    if state.active_tool_pause().is_some() {
        handle_tool_pause_key(state, code, request_tx).await;
        return true;
    }

    if is_profile_toggle_key(code, modifiers) {
        let _ = request_tx.send(UiToRuntimeEvent::ToggleActiveProfile).await;
        return true;
    }

    if state.autocomplete.visible {
        handle_command_autocomplete_key(state, code, modifiers, request_tx).await;
        return true;
    }

    if state.mention_autocomplete.visible {
        handle_mention_autocomplete_key(state, code, modifiers);
        return true;
    }

    handle_composer_key(state, code, modifiers, request_tx).await
}

fn handle_help_drawer_key(state: &mut UiState, code: KeyCode, modifiers: KeyModifiers) {
    match (code, modifiers) {
        (KeyCode::Esc, _) => state.close_help_drawer(),
        (KeyCode::Right, _) => state.help_next_tab(),
        (KeyCode::Tab, modifiers) if modifiers.is_empty() => state.help_next_tab(),
        (KeyCode::Left, _) | (KeyCode::BackTab, _) => state.help_prev_tab(),
        (KeyCode::Tab, KeyModifiers::SHIFT) => state.help_prev_tab(),
        (KeyCode::Down, _) | (KeyCode::Char('j'), _) => state.help_select_next(),
        (KeyCode::Up, _) | (KeyCode::Char('k'), _) => state.help_select_prev(),
        (KeyCode::PageDown, _) => {
            let page = state
                .help_drawer
                .as_ref()
                .map(|_| state.messages_area.height as usize / 2)
                .unwrap_or(1)
                .max(1);
            state.help_page_down(page);
        }
        (KeyCode::PageUp, _) => {
            let page = state
                .help_drawer
                .as_ref()
                .map(|_| state.messages_area.height as usize / 2)
                .unwrap_or(1)
                .max(1);
            state.help_page_up(page);
        }
        _ => {}
    }
}

fn is_profile_toggle_key(code: KeyCode, modifiers: KeyModifiers) -> bool {
    code == KeyCode::BackTab || (code == KeyCode::Tab && modifiers.contains(KeyModifiers::SHIFT))
}

async fn handle_plan_approval_key(
    state: &mut UiState,
    code: KeyCode,
    request_tx: &mpsc::Sender<UiToRuntimeEvent>,
) {
    let Some(plan) = state.plan_approval.as_ref() else {
        return;
    };
    match code {
        KeyCode::Up | KeyCode::Char('k') => {
            state.plan_approval_selected = state.plan_approval_selected.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            state.plan_approval_selected = (state.plan_approval_selected + 1).min(2);
        }
        KeyCode::Char('1') => {
            state.plan_approval_selected = 0;
            submit_plan_approval(state, request_tx).await;
        }
        KeyCode::Char('2') => {
            state.plan_approval_selected = 1;
            submit_plan_approval(state, request_tx).await;
        }
        KeyCode::Char('3') => {
            state.plan_approval_selected = 2;
            submit_plan_approval(state, request_tx).await;
        }
        KeyCode::Esc => {
            let plan_id = plan.id.clone();
            state.plan_approval = None;
            state.plan_approval_selected = 0;
            let _ = request_tx
                .send(UiToRuntimeEvent::ResolvePlanApproval {
                    plan_id,
                    action: PlanApprovalAction::ContinueDiscussing,
                })
                .await;
        }
        KeyCode::Enter => {
            submit_plan_approval(state, request_tx).await;
        }
        _ => {}
    }
}

async fn submit_plan_approval(state: &mut UiState, request_tx: &mpsc::Sender<UiToRuntimeEvent>) {
    let Some(plan) = state.plan_approval.take() else {
        return;
    };
    let action = match state.plan_approval_selected.min(2) {
        0 => PlanApprovalAction::Approve,
        1 => PlanApprovalAction::ApproveAndCompact,
        _ => PlanApprovalAction::ContinueDiscussing,
    };
    state.plan_approval_selected = 0;
    let _ = request_tx
        .send(UiToRuntimeEvent::ResolvePlanApproval {
            plan_id: plan.id,
            action,
        })
        .await;
}

async fn handle_tool_pause_key(
    state: &mut UiState,
    code: KeyCode,
    request_tx: &mpsc::Sender<UiToRuntimeEvent>,
) {
    let Some(active_pause) = state.active_tool_pause().cloned() else {
        return;
    };
    let user_input_option_max = match &active_pause.kind {
        ToolPauseKind::UserInput(preview) => preview
            .questions
            .get(state.user_input_question_index)
            .map(|question| question.options.len())
            .unwrap_or(0),
        ToolPauseKind::Permission(_) => 1,
    };
    let is_permission_pause = matches!(&active_pause.kind, ToolPauseKind::Permission(_));
    let is_user_input_pause = matches!(&active_pause.kind, ToolPauseKind::UserInput(_));

    if state.user_input_note_mode {
        match code {
            KeyCode::Tab | KeyCode::Esc => state.user_input_note_mode = false,
            KeyCode::Enter if is_permission_pause => {
                state.permission_selected = 1;
                input::resolve_active_tool_pause(state, request_tx).await;
            }
            KeyCode::Enter => {
                state.mark_current_user_input_answered();
                if state.user_input_unanswered_count() == 0 {
                    input::resolve_active_tool_pause(state, request_tx).await;
                } else {
                    state.move_to_next_unanswered_user_input();
                }
            }
            KeyCode::Backspace => state.delete_note_before(),
            KeyCode::Delete => state.delete_note_after(),
            KeyCode::Up | KeyCode::Char('k') if is_user_input_pause => {
                state.permission_select_prev()
            }
            KeyCode::Down | KeyCode::Char('j') if is_user_input_pause => {
                state.permission_select_next_with_max(user_input_option_max);
            }
            KeyCode::Char(c) => state.insert_note_char(c),
            KeyCode::Left => state.note_cursor_left(),
            KeyCode::Right => state.note_cursor_right(),
            KeyCode::Home => state.note_cursor_home(),
            KeyCode::End => state.note_cursor_end(),
            _ => {}
        }
        return;
    }

    match code {
        KeyCode::Up | KeyCode::Char('k') => state.permission_select_prev(),
        KeyCode::Down | KeyCode::Char('j') => {
            state.permission_select_next_with_max(user_input_option_max);
        }
        KeyCode::Left | KeyCode::Char('h') if is_user_input_pause => {
            state.user_input_question_prev();
        }
        KeyCode::Right | KeyCode::Char('l') if is_user_input_pause => {
            state.user_input_question_next();
        }
        KeyCode::PageUp => {
            let page = 1.max(state.permission_drawer_body_area.height as usize / 2);
            state.permission_scroll_up(page);
        }
        KeyCode::PageDown => {
            let page = 1.max(state.permission_drawer_body_area.height as usize / 2);
            state.permission_scroll_down(page);
        }
        KeyCode::Tab if is_permission_pause => {
            state.permission_selected = 1;
            state.user_input_note_mode = true;
            state.note_cursor_end();
        }
        KeyCode::Tab if is_user_input_pause => {
            state.user_input_note_mode = true;
            state.note_cursor_end();
        }
        KeyCode::Char('y') | KeyCode::Char('Y') if is_permission_pause => {
            state.permission_selected = 0;
            input::resolve_active_tool_pause(state, request_tx).await;
        }
        KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') if is_permission_pause => {
            state.permission_selected = 1;
            input::resolve_active_tool_pause(state, request_tx).await;
        }
        KeyCode::Esc => {
            state.permission_selected = 1;
            let _ = request_tx
                .send(UiToRuntimeEvent::ResolveToolPause {
                    tool_use_id: active_pause.tool_use_id.clone(),
                    response: ToolPauseResponse::Cancelled,
                })
                .await;
            state
                .pending_tool_previews
                .remove(&active_pause.tool_use_id);
            if state.pending_tool_previews.is_empty() {
                state.resume_run_timer();
                state.reset_permission_drawer();
            }
        }
        KeyCode::Enter => {
            if matches!(active_pause.kind, ToolPauseKind::UserInput(_)) {
                state.mark_current_user_input_answered();
                if state.user_input_unanswered_count() == 0 {
                    input::resolve_active_tool_pause(state, request_tx).await;
                } else {
                    state.move_to_next_unanswered_user_input();
                }
            } else {
                input::resolve_active_tool_pause(state, request_tx).await;
            }
        }
        _ => {}
    }
}

async fn handle_command_autocomplete_key(
    state: &mut UiState,
    code: KeyCode,
    modifiers: KeyModifiers,
    request_tx: &mpsc::Sender<UiToRuntimeEvent>,
) {
    if input::is_newline_key(code, modifiers) {
        state.insert_text("\n");
        state.update_input_autocomplete();
        return;
    }

    match code {
        KeyCode::Enter | KeyCode::Tab => {
            if let Some(cmd) = state.autocomplete.selected_command().cloned() {
                if cmd.has_args {
                    state.input = format!("/{} ", cmd.name);
                    state.input_mentions.clear();
                    state.input_paste_markers.clear();
                    state.input_scroll_line = 0;
                    state.cursor_char = state.input.chars().count();
                    state.autocomplete.visible = false;
                } else {
                    state.autocomplete.visible = false;
                    state.input = format!("/{}", cmd.name);
                    state.input_mentions.clear();
                    state.input_paste_markers.clear();
                    state.input_scroll_line = 0;
                    let msg = std::mem::take(&mut state.input);
                    state.cursor_char = 0;
                    if !msg.is_empty() {
                        let _ = request_tx.send(UiToRuntimeEvent::SendCommand(msg)).await;
                    }
                }
            }
            state.autocomplete.visible = false;
        }
        KeyCode::Down => state.autocomplete.select_next(),
        KeyCode::Up => state.autocomplete.select_prev(),
        KeyCode::Esc => state.autocomplete.visible = false,
        KeyCode::Backspace => {
            state.delete_before();
            state.update_input_autocomplete();
        }
        KeyCode::Delete => {
            state.delete_after();
            state.update_input_autocomplete();
        }
        KeyCode::Char(c) => {
            state.insert_char(c);
            state.update_input_autocomplete();
        }
        KeyCode::Left => {
            state.cursor_left();
            state.update_input_autocomplete();
        }
        KeyCode::Right => {
            state.cursor_right();
            state.update_input_autocomplete();
        }
        KeyCode::Home => {
            state.cursor_home();
            state.update_input_autocomplete();
        }
        KeyCode::End => {
            state.cursor_end();
            state.update_input_autocomplete();
        }
        _ => {}
    }
}

fn handle_mention_autocomplete_key(state: &mut UiState, code: KeyCode, modifiers: KeyModifiers) {
    if input::is_newline_key(code, modifiers) {
        state.insert_text("\n");
        state.update_input_autocomplete();
        return;
    }

    match code {
        KeyCode::Enter => {
            state.insert_selected_mention();
            state.update_input_autocomplete();
        }
        KeyCode::Tab | KeyCode::Right => {
            state.expand_selected_mention_directory();
            state.update_input_autocomplete();
        }
        KeyCode::Down => state.mention_autocomplete.select_next(),
        KeyCode::Up => state.mention_autocomplete.select_prev(),
        KeyCode::Esc => state.cancel_mention_autocomplete(),
        KeyCode::Backspace => {
            state.delete_before();
            state.update_input_autocomplete();
        }
        KeyCode::Delete => {
            state.delete_after();
            state.update_input_autocomplete();
        }
        KeyCode::Char(c) => {
            state.insert_char(c);
            state.update_input_autocomplete();
        }
        KeyCode::Left => {
            state.cursor_left();
            state.update_input_autocomplete();
        }
        KeyCode::Home => {
            state.cursor_home();
            state.update_input_autocomplete();
        }
        KeyCode::End => {
            state.cursor_end();
            state.update_input_autocomplete();
        }
        _ => {}
    }
}

async fn handle_composer_key(
    state: &mut UiState,
    code: KeyCode,
    modifiers: KeyModifiers,
    request_tx: &mpsc::Sender<UiToRuntimeEvent>,
) -> bool {
    let page_amt = 1.max(state.messages_area.height as usize / 2);
    match (code, modifiers) {
        (KeyCode::Char('c'), KeyModifiers::CONTROL) | (KeyCode::Char('\x03'), _) => {
            return state.clear_input();
        }
        (KeyCode::Up, _) => {
            state.cursor_up_in_input();
        }
        (KeyCode::Down, _) => {
            state.cursor_down_in_input();
        }
        (KeyCode::PageUp, _) => {
            state.update_scroll_step(tokio::time::Instant::now());
            state.scroll_up(state.scroll_step.max(page_amt));
        }
        (KeyCode::PageDown, _) => {
            state.update_scroll_step(tokio::time::Instant::now());
            state.scroll_down(state.scroll_step.max(page_amt));
        }
        (code, modifiers) if input::is_intervention_key(code, modifiers) => {
            input::submit_queued_intervention(state, request_tx).await;
        }
        (code, modifiers) if input::is_newline_key(code, modifiers) => {
            state.insert_text("\n");
            state.update_input_autocomplete();
        }
        (KeyCode::Enter, _) => {
            if !state.pending_intervention_inputs.is_empty() {
                return true;
            }

            if let Some(draft) = state.take_input_draft() {
                if draft.text.starts_with('/') {
                    let _ = request_tx
                        .send(UiToRuntimeEvent::SendCommand(draft.text))
                        .await;
                } else if state.is_run_active() {
                    state.queued_user_inputs.push_back(draft);
                } else {
                    state.clear_run_dividers();
                    let ui_message = match draft.clone().history_item() {
                        crate::types::display::HistoryItem::Message(message) => {
                            UiMessage::Message(message)
                        }
                        crate::types::display::HistoryItem::Display(display) => {
                            UiMessage::Display(display)
                        }
                        crate::types::display::HistoryItem::Plan(plan) => UiMessage::ProposedPlan {
                            text: plan.markdown,
                        },
                    };
                    state.messages.push(ui_message);
                    let _ = request_tx.send(UiToRuntimeEvent::SendMessage(draft)).await;
                    state.scroll_offset = 0;
                    state.auto_scroll = true;
                    state.agent_status = AgentStatus::Working;
                }
            }
        }
        (KeyCode::Backspace, _) => {
            state.delete_before();
            state.update_input_autocomplete();
        }
        (KeyCode::Delete, _) => {
            state.delete_after();
            state.update_input_autocomplete();
        }
        (KeyCode::Char(c), _) => {
            state.insert_char(c);
            state.update_input_autocomplete();
        }
        (KeyCode::Left, _) => {
            state.cursor_left();
            state.update_input_autocomplete();
        }
        (KeyCode::Right, _) => {
            state.cursor_right();
            state.update_input_autocomplete();
        }
        (KeyCode::Home, KeyModifiers::CONTROL) => state.scroll_to_top(),
        (KeyCode::End, KeyModifiers::CONTROL) => state.scroll_to_bottom(),
        (KeyCode::Home, _) => {
            state.cursor_home();
            state.update_input_autocomplete();
        }
        (KeyCode::End, _) => {
            state.cursor_end();
            state.update_input_autocomplete();
        }
        _ => {}
    }
    true
}

fn handle_mouse_event(state: &mut UiState, kind: MouseEventKind, row: u16, column: u16) {
    if state.is_selecting_text {
        match kind {
            MouseEventKind::Drag(MouseButton::Left) => {
                update_text_selection_from_mouse(state, row, column);
            }
            MouseEventKind::Up(MouseButton::Left) => {
                update_text_selection_from_mouse(state, row, column);
                state.is_selecting_text = false;
                if let Some(text) = selected_text(state) {
                    copy_to_clipboard(&text);
                }
                state.text_selection = None;
            }
            _ => {}
        }
        return;
    }

    if state.active_tool_pause().is_some() {
        let drawer = state.permission_drawer_area;
        let body = state.permission_drawer_body_area;
        let in_drawer = row >= drawer.top()
            && row < drawer.bottom()
            && column >= drawer.left()
            && column < drawer.right();
        let in_body = row >= body.top()
            && row < body.bottom()
            && column >= body.left()
            && column < body.right();

        match kind {
            MouseEventKind::Down(MouseButton::Left) if in_drawer => {
                if row == drawer.bottom().saturating_sub(2) {
                    state.permission_selected = 0;
                } else if row == drawer.bottom().saturating_sub(1) {
                    state.permission_selected = 1;
                }
            }
            MouseEventKind::ScrollUp if in_body => {
                state.update_scroll_step(tokio::time::Instant::now());
                state.permission_scroll_up(state.scroll_step);
            }
            MouseEventKind::ScrollDown if in_body => {
                state.update_scroll_step(tokio::time::Instant::now());
                state.permission_scroll_down(state.scroll_step);
            }
            _ => {}
        }
        return;
    }

    match kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if let Some(point) = selection_point_from_mouse(state, row, column) {
                state.text_selection = Some(TextSelection {
                    start: point,
                    end: point,
                });
                state.is_selecting_text = true;
            }
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            update_text_selection_from_mouse(state, row, column);
        }
        MouseEventKind::Up(MouseButton::Left) => {
            update_text_selection_from_mouse(state, row, column);
            state.is_selecting_text = false;
            if let Some(text) = selected_text(state) {
                copy_to_clipboard(&text);
            }
            state.text_selection = None;
        }
        MouseEventKind::ScrollUp => {
            state.update_scroll_step(tokio::time::Instant::now());
            state.scroll_up(state.scroll_step);
        }
        MouseEventKind::ScrollDown => {
            state.update_scroll_step(tokio::time::Instant::now());
            state.scroll_down(state.scroll_step);
        }
        _ => {}
    }
}
