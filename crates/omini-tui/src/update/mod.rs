use super::clipboard::copy_to_clipboard;
use super::input;
use super::selection::{
    selected_text, selection_point_from_mouse, update_text_selection_from_mouse,
};
use super::state::{AgentStatus, TextSelection, UiMessage, UiState};
use crate::types::events::{RuntimeToUiEvent, ToolPauseKind, ToolPauseResponse, UiToRuntimeEvent};
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
            if state.interaction_step.is_none() && state.active_tool_pause().is_none() =>
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

    if state.user_input_note_mode {
        match code {
            KeyCode::Tab | KeyCode::Esc => state.user_input_note_mode = false,
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
            KeyCode::Up | KeyCode::Char('k') => state.permission_select_prev(),
            KeyCode::Down | KeyCode::Char('j') => {
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
        KeyCode::Left | KeyCode::Char('h')
            if matches!(active_pause.kind, ToolPauseKind::UserInput(_)) =>
        {
            state.user_input_question_prev();
        }
        KeyCode::Right | KeyCode::Char('l')
            if matches!(active_pause.kind, ToolPauseKind::UserInput(_)) =>
        {
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
        KeyCode::Tab if matches!(active_pause.kind, ToolPauseKind::UserInput(_)) => {
            state.user_input_note_mode = true;
            state.note_cursor_end();
        }
        KeyCode::Char('y') | KeyCode::Char('Y')
            if matches!(active_pause.kind, ToolPauseKind::Permission(_)) =>
        {
            state.permission_selected = 0;
            input::resolve_active_tool_pause(state, request_tx).await;
        }
        KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N')
            if matches!(active_pause.kind, ToolPauseKind::Permission(_)) =>
        {
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
            state.reset_permission_drawer();
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
                    let ui_message = match draft.clone().history_item() {
                        crate::types::display::HistoryItem::Message(message) => {
                            UiMessage::Message(message)
                        }
                        crate::types::display::HistoryItem::Display(display) => {
                            UiMessage::Display(display)
                        }
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
