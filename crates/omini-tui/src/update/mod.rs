use super::clipboard::copy_to_clipboard;
use super::command::INIT_PROMPT;
use super::input;
use super::protocol;
use super::selection::{
    selected_text, selection_point_from_mouse, update_text_selection_from_mouse,
};
use super::state::{AgentStatus, TextSelection, UiMessage, UiState};
use crate::client::ClientRequest;
use crate::types::events::{
    ActiveProfile, PermissionPreview, PlanApprovalAction, PlanExecutionProfile, RuntimeToUiEvent,
    ToolPauseKind,
};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use omini_domain::display::UserDraft;
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
    request_tx: &mpsc::Sender<ClientRequest>,
) -> UpdateOutcome {
    match event {
        Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
            if handle_key_event(state, key.code, key.modifiers, request_tx).await {
                UpdateOutcome::redraw()
            } else {
                UpdateOutcome::exit()
            }
        }
        Event::Paste(text) if state.user_input_note_mode => {
            // note 模式期间放行终端 bracketed paste(Shift+Insert 等),
            // 逐字符塞进 note。修复 Bug 2 的一部分:此前主输入框分支的
            // `active_tool_pause().is_none()` 守卫把 tool pause 期间的
            // `Event::Paste` 整体吞掉,note 模式无处可粘。
            for c in text.chars() {
                state.insert_note_char(c);
            }
            UpdateOutcome::redraw()
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
    request_tx: &mpsc::Sender<ClientRequest>,
) -> UpdateOutcome {
    if let RuntimeToUiEvent::InteractionRequest(ref req) = event {
        state.open_interaction_request(req);
    }

    if matches!(event, RuntimeToUiEvent::Shutdown) {
        return UpdateOutcome::exit();
    }

    if let RuntimeToUiEvent::SessionSnapshot {
        session_id,
        messages,
        subagents,
        usage,
    } = event
    {
        state.apply_session_snapshot(session_id, messages, subagents, usage);
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
    request_tx: &mpsc::Sender<ClientRequest>,
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
    request_tx: &mpsc::Sender<ClientRequest>,
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
        let _ = request_tx.send(ClientRequest::RunCancel).await;
        return true;
    }

    if state.active_tool_pause().is_some() {
        handle_tool_pause_key(state, code, request_tx).await;
        return true;
    }

    if is_profile_toggle_key(code, modifiers) {
        let _ = request_tx.send(ClientRequest::ProfileToggle).await;
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
    request_tx: &mpsc::Sender<ClientRequest>,
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
        KeyCode::Char('a') | KeyCode::Char('A') => {
            state.plan_approval_auto = !state.plan_approval_auto;
        }
        KeyCode::Esc => {
            let plan_id = plan.id.clone();
            state.plan_approval = None;
            state.plan_approval_selected = 0;
            state.plan_approval_auto = false;
            let _ = request_tx
                .send(ClientRequest::PlanResolve {
                    plan_id,
                    action: omini_protocol::PlanApprovalAction::ContinueDiscussing,
                })
                .await;
        }
        KeyCode::Enter => {
            submit_plan_approval(state, request_tx).await;
        }
        _ => {}
    }
}

async fn submit_plan_approval(state: &mut UiState, request_tx: &mpsc::Sender<ClientRequest>) {
    let Some(plan) = state.plan_approval.take() else {
        return;
    };
    let profile = if state.plan_approval_auto {
        PlanExecutionProfile::Auto
    } else {
        PlanExecutionProfile::Main
    };
    let action = match state.plan_approval_selected.min(2) {
        0 => PlanApprovalAction::Approve { profile },
        1 => PlanApprovalAction::ApproveInNewSession { profile },
        _ => PlanApprovalAction::ContinueDiscussing,
    };
    state.plan_approval_selected = 0;
    state.plan_approval_auto = false;
    let _ = request_tx
        .send(ClientRequest::PlanResolve {
            plan_id: plan.id,
            action: protocol::plan_approval_action_from_internal(action),
        })
        .await;
}

async fn handle_tool_pause_key(
    state: &mut UiState,
    code: KeyCode,
    request_tx: &mpsc::Sender<ClientRequest>,
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
            // note 模式是纯文本输入,`j` / `k` 不再被当成方向键别名,
            // 必须能作为普通字符插入;方向键导航仍由 `Up` / `Down` 负责。
            // 修复 Bug 1:之前 `KeyCode::Char('k')` / `KeyCode::Char('j')`
            // 在 `if is_user_input_pause` 守卫下吞掉了 ask_user note 模式
            // 中的 `j` / `k` 字符。
            KeyCode::Up if is_user_input_pause => state.permission_select_prev(),
            KeyCode::Down if is_user_input_pause => {
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
                .send(ClientRequest::ToolPauseResolve {
                    tool_use_id: active_pause.tool_use_id.clone(),
                    response: omini_protocol::ToolPauseResponse::Cancelled,
                })
                .await;
            let removed_active = state.remove_tool_pause(&active_pause.tool_use_id);
            state.finish_tool_pause_removal(removed_active);
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
    request_tx: &mpsc::Sender<ClientRequest>,
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
                        let draft = omini_domain::display::UserDraft::plain(msg);
                        if let Some(request) = request_from_command_draft(state, draft) {
                            let _ = request_tx.send(request).await;
                        }
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

fn is_compact_command(text: &str) -> bool {
    let Some(rest) = text.trim().strip_prefix('/') else {
        return false;
    };
    rest.split_whitespace().next() == Some("compact")
}

fn parse_slash_command(text: &str) -> Option<(String, String)> {
    let rest = text.trim().strip_prefix('/')?;
    if rest.is_empty() {
        return None;
    }
    let (name, args) = match rest.split_once(char::is_whitespace) {
        Some((name, args)) => (name, args.trim()),
        None => (rest, ""),
    };
    Some((name.to_ascii_lowercase(), args.to_string()))
}

fn request_from_command_draft(state: &mut UiState, draft: UserDraft) -> Option<ClientRequest> {
    let Some((name, args)) = parse_slash_command(&draft.text) else {
        return Some(ClientRequest::RunSubmitUserInput {
            input: protocol::user_input_from_draft(draft),
            client_echo_id: None,
        });
    };

    match name.as_str() {
        "exit" | "quit" => Some(ClientRequest::AppShutdown),
        "help" | "?" => {
            state.open_help_drawer(state.autocomplete.all_commands.clone());
            None
        }
        "model" => Some(ClientRequest::OpenModelPicker),
        "sessions" | "resume" => Some(ClientRequest::OpenSessionPicker),
        "agents" => Some(ClientRequest::OpenAgentManager),
        "new" | "clear" => Some(ClientRequest::SessionNew {
            profile: protocol::active_profile_from_internal(state.status_bar.active_profile),
        }),
        "plan" => Some(ClientRequest::ProfileSet {
            profile: protocol::active_profile_from_internal(ActiveProfile::Plan),
        }),
        "compact" => Some(ClientRequest::ContextCompact {
            instructions: (!args.is_empty()).then_some(args),
        }),
        "rename" => Some(ClientRequest::SessionRename { title: args }),
        "init" => {
            let mut input = protocol::user_input_from_draft(draft);
            let mut prompt = INIT_PROMPT.to_string();
            if !args.is_empty() {
                prompt.push_str("\n\nAdditional user notes for this initialization:\n");
                prompt.push_str(&args);
            }
            input.text = prompt;
            Some(ClientRequest::RunSubmitUserInput {
                input,
                client_echo_id: None,
            })
        }
        "thinking" => match args.as_str() {
            "" => Some(ClientRequest::ThinkingDisplaySet { show: None }),
            "on" => Some(ClientRequest::ThinkingDisplaySet { show: Some(true) }),
            "off" => Some(ClientRequest::ThinkingDisplaySet { show: Some(false) }),
            _ => {
                state.apply_event(RuntimeToUiEvent::error(format!(
                    "无效的 thinking 展示设置 '{}'，可用值: on | off",
                    args
                )));
                None
            }
        },
        "effort" => match args.parse() {
            Ok(effort) => Some(ClientRequest::ModelThinkingEffortSet { effort }),
            Err(()) => {
                state.apply_event(RuntimeToUiEvent::error(format!(
                    "无效的思考程度 '{}'，可用值: none | low | medium | high | xhigh | max",
                    args
                )));
                None
            }
        },
        skill_name => Some(ClientRequest::ExpandSkillRun {
            skill_name: skill_name.to_string(),
            prompt: args,
            input: Some(protocol::user_input_from_draft(draft)),
        }),
    }
}

async fn handle_composer_key(
    state: &mut UiState,
    code: KeyCode,
    modifiers: KeyModifiers,
    request_tx: &mpsc::Sender<ClientRequest>,
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
                    if !state.is_run_active() && is_compact_command(&draft.text) {
                        state.begin_manual_compact();
                    }
                    if let Some(request) = request_from_command_draft(state, draft) {
                        let _ = request_tx.send(request).await;
                    }
                } else if state.is_run_active() && !state.manual_compact_running {
                    state.queued_user_inputs.push_back(draft);
                } else {
                    state.clear_run_dividers();
                    state.show_start_screen = false;
                    let ui_message = match draft.clone().history_item() {
                        omini_domain::display::HistoryItem::Message(message) => {
                            UiMessage::Message(message)
                        }
                        omini_domain::display::HistoryItem::Display(display) => {
                            UiMessage::Display(display)
                        }
                        omini_domain::display::HistoryItem::Plan(plan) => UiMessage::ProposedPlan {
                            text: plan.markdown,
                        },
                        omini_domain::display::HistoryItem::Summary(summary) => {
                            UiMessage::CompactSummary {
                                text: summary.markdown,
                            }
                        }
                    };
                    let client_echo_id = uuid::Uuid::new_v4().to_string();
                    state.push_optimistic_echo(ui_message, client_echo_id.clone());
                    let _ = request_tx
                        .send(ClientRequest::RunSubmitUserInput {
                            input: protocol::user_input_from_draft(draft),
                            client_echo_id: Some(client_echo_id),
                        })
                        .await;
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

    if active_permission_drawer_captures_scroll(state) {
        match kind {
            MouseEventKind::ScrollUp => {
                state.update_scroll_step(tokio::time::Instant::now());
                state.permission_scroll_up(state.scroll_step);
                return;
            }
            MouseEventKind::ScrollDown => {
                state.update_scroll_step(tokio::time::Instant::now());
                state.permission_scroll_down(state.scroll_step);
                return;
            }
            _ => {}
        }
    }

    if state.active_tool_pause().is_some() {
        let drawer = state.permission_drawer_area;
        let in_drawer = row >= drawer.top()
            && row < drawer.bottom()
            && column >= drawer.left()
            && column < drawer.right();

        let in_action_row =
            row == drawer.bottom().saturating_sub(2) || row == drawer.bottom().saturating_sub(1);
        match kind {
            MouseEventKind::Down(MouseButton::Left) if in_drawer && in_action_row => {
                if row == drawer.bottom().saturating_sub(2) {
                    state.permission_selected = 0;
                } else {
                    state.permission_selected = 1;
                }
                return;
            }
            _ if in_drawer => {}
            _ => {}
        }
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

fn active_permission_drawer_captures_scroll(state: &UiState) -> bool {
    let Some(request) = state.active_tool_pause() else {
        return false;
    };
    let is_large_file_preview = matches!(
        &request.kind,
        ToolPauseKind::Permission(PermissionPreview::Bash(_))
            | ToolPauseKind::Permission(PermissionPreview::Edit(_))
            | ToolPauseKind::Permission(PermissionPreview::Write(_))
            | ToolPauseKind::Permission(PermissionPreview::Mcp(_))
    );
    is_large_file_preview
        && state.permission_drawer_body_area.height > 0
        && state.permission_drawer_content_len > state.permission_drawer_body_area.height as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::InputMention;
    use crate::types::events::{
        ActiveProfile, EditPermissionPreview, PermissionPreview, SubmittedPlan, ToolPauseRequest,
        UserInputOption, UserInputPreview, UserInputQuestion,
    };
    use chrono::Utc;
    use crossterm::event::KeyEvent;
    use omini_domain::display::MentionKind;
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

    fn edit_permission_pause(tool_use_id: &str) -> ToolPauseRequest {
        ToolPauseRequest {
            tool_use_id: tool_use_id.to_string(),
            preview_tool_use_id: None,
            tool_name: "edit".to_string(),
            permission_source: None,
            source_session_id: None,
            source_agent_label: None,
            kind: ToolPauseKind::Permission(PermissionPreview::Edit(EditPermissionPreview {
                summary: "Edit /tmp/demo.rs".to_string(),
                path: "/tmp/demo.rs".to_string(),
                replacement_count: 1,
                replace_all: false,
                start_lines: vec![1],
                added_lines: 1,
                removed_lines: 1,
            })),
        }
    }

    fn state_with_permission_pause() -> UiState {
        let mut state = UiState::new();
        state.apply_event(RuntimeToUiEvent::ToolPauseRequested(permission_pause(
            "tool_1",
        )));
        state
    }

    fn user_input_pause(tool_use_id: &str) -> ToolPauseRequest {
        ToolPauseRequest {
            tool_use_id: tool_use_id.to_string(),
            preview_tool_use_id: None,
            tool_name: "ask_user".to_string(),
            permission_source: None,
            source_session_id: None,
            source_agent_label: None,
            kind: ToolPauseKind::UserInput(UserInputPreview {
                questions: vec![UserInputQuestion {
                    id: "q1".to_string(),
                    header: "header".to_string(),
                    question: "Pick one".to_string(),
                    options: (0..3)
                        .map(|i| UserInputOption {
                            label: format!("opt{i}"),
                            description: String::new(),
                        })
                        .collect(),
                }],
            }),
        }
    }

    fn state_with_user_input_pause() -> UiState {
        let mut state = UiState::new();
        state.apply_event(RuntimeToUiEvent::ToolPauseRequested(user_input_pause(
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

    async fn recv_pause_response(
        rx: &mut mpsc::Receiver<ClientRequest>,
    ) -> omini_protocol::ToolPauseResponse {
        let Some(ClientRequest::ToolPauseResolve { response, .. }) = rx.recv().await else {
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

    // 修复 Bug 1 的回归测试:ask_user 暂停进入 note 模式后,`j` / `k`
    // 必须是普通字符插入,不能被 vim 风格方向键别名吞掉。
    #[tokio::test]
    async fn user_input_note_mode_inserts_jk_as_text() {
        let mut state = state_with_user_input_pause();
        let (tx, _rx) = mpsc::channel(1);

        // Tab 切换到 note 模式
        handle_tool_pause_key(&mut state, KeyCode::Tab, &tx).await;
        assert!(state.user_input_note_mode);
        let initial_selected = state.current_user_input_selected();

        for c in "skip、kick this off".chars() {
            handle_tool_pause_key(&mut state, KeyCode::Char(c), &tx).await;
        }

        assert_eq!(state.current_user_input_note(), "skip、kick this off");
        assert_eq!(state.current_user_input_selected(), initial_selected);
    }

    // 修复 Bug 1 的回归测试:note 模式下方向键 `↑` / `↓` 仍要能切选项。
    // 仅验证"切换选项"语义,saturate 边界(max_selected 取 options.len()
    // 而非 len-1 导致的 off-by-one)与本 issue 无关,不在此测试覆盖。
    #[tokio::test]
    async fn user_input_note_mode_arrows_still_navigate_options() {
        let mut state = state_with_user_input_pause();
        let (tx, _rx) = mpsc::channel(1);

        handle_tool_pause_key(&mut state, KeyCode::Tab, &tx).await;
        assert!(state.user_input_note_mode);
        assert_eq!(state.current_user_input_selected(), 0);

        handle_tool_pause_key(&mut state, KeyCode::Down, &tx).await;
        assert_eq!(state.current_user_input_selected(), 1);
        handle_tool_pause_key(&mut state, KeyCode::Up, &tx).await;
        assert_eq!(state.current_user_input_selected(), 0);
    }

    // Permission 暂停的 note 模式回归基线:`j` / `k` 原本就能插入,
    // 此处固定当前行为,防止后续改动破坏非 ask_user 路径。
    #[tokio::test]
    async fn permission_note_mode_inserts_jk_as_text() {
        let mut state = state_with_permission_pause();
        let (tx, _rx) = mpsc::channel(1);

        handle_tool_pause_key(&mut state, KeyCode::Tab, &tx).await;
        assert!(state.user_input_note_mode);

        for c in "jk".chars() {
            handle_tool_pause_key(&mut state, KeyCode::Char(c), &tx).await;
        }

        assert_eq!(state.current_user_input_note(), "jk");
    }

    // 修复 Bug 2 的回归测试:note 模式期间 `Event::Paste` 走
    // `handle_input_event` 中独立的 note 分支,把每个字符塞进 note。
    // 之前主输入框分支的 `active_tool_pause().is_none()` 守卫把整条
    // bracketed paste 路径吞掉,note 模式无处可粘。
    #[tokio::test]
    async fn user_input_note_mode_paste_inserts_chars() {
        let mut state = state_with_user_input_pause();
        let (tx, _rx) = mpsc::channel(1);

        handle_tool_pause_key(&mut state, KeyCode::Tab, &tx).await;
        assert!(state.user_input_note_mode);

        let outcome =
            handle_input_event(&mut state, Event::Paste("pasted note".to_string()), &tx).await;
        assert!(outcome.redraw);

        assert_eq!(state.current_user_input_note(), "pasted note");
    }

    #[tokio::test]
    async fn permission_note_mode_paste_inserts_chars() {
        let mut state = state_with_permission_pause();
        let (tx, _rx) = mpsc::channel(1);

        handle_tool_pause_key(&mut state, KeyCode::Tab, &tx).await;
        assert!(state.user_input_note_mode);

        let outcome = handle_input_event(&mut state, Event::Paste("hi".to_string()), &tx).await;
        assert!(outcome.redraw);

        assert_eq!(state.current_user_input_note(), "hi");
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
            omini_protocol::ToolPauseResponse::Permission {
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
            omini_protocol::ToolPauseResponse::Permission {
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
            omini_protocol::ToolPauseResponse::Permission {
                approved: true,
                note: None,
            }
        );
        assert_eq!(state.agent_status, AgentStatus::Working);
    }

    #[test]
    fn active_permission_pause_allows_message_text_selection_outside_drawer() {
        let mut state = state_with_permission_pause();
        state.register_selectable_screen_line(2, 0, 80, "assistant line".to_string());
        state.permission_drawer_area = ratatui::layout::Rect::new(0, 10, 80, 6);
        state.permission_drawer_body_area = ratatui::layout::Rect::new(3, 12, 74, 2);

        handle_mouse_event(&mut state, MouseEventKind::Down(MouseButton::Left), 2, 0);

        assert!(state.is_selecting_text);
        assert!(state.text_selection.is_some());
    }

    #[test]
    fn active_permission_pause_allows_drawer_text_selection() {
        let mut state = state_with_permission_pause();
        state.register_selectable_screen_line(12, 3, 74, "drawer line".to_string());
        state.permission_drawer_area = ratatui::layout::Rect::new(0, 10, 80, 6);
        state.permission_drawer_body_area = ratatui::layout::Rect::new(3, 12, 74, 2);

        handle_mouse_event(&mut state, MouseEventKind::Down(MouseButton::Left), 12, 3);

        assert!(state.is_selecting_text);
        assert!(state.text_selection.is_some());
    }

    #[test]
    fn scrollable_edit_permission_drawer_captures_mouse_wheel() {
        let mut state = UiState::new();
        state.apply_event(RuntimeToUiEvent::ToolPauseRequested(edit_permission_pause(
            "tool_1",
        )));
        state.permission_drawer_area = ratatui::layout::Rect::new(0, 10, 80, 6);
        state.permission_drawer_body_area = ratatui::layout::Rect::new(3, 12, 74, 2);
        state.permission_drawer_content_len = 8;
        state.permission_scroll_offset = 0;

        handle_mouse_event(&mut state, MouseEventKind::ScrollUp, 2, 0);

        assert!(state.permission_scroll_offset > 0);
        assert_eq!(state.scroll_offset, 0);
        assert!(state.auto_scroll);
    }

    #[test]
    fn non_scrollable_or_non_edit_permission_drawer_uses_message_wheel() {
        let mut state = UiState::new();
        state.apply_event(RuntimeToUiEvent::ToolPauseRequested(edit_permission_pause(
            "tool_1",
        )));
        state.permission_drawer_area = ratatui::layout::Rect::new(0, 10, 80, 6);
        state.permission_drawer_body_area = ratatui::layout::Rect::new(3, 12, 74, 2);
        state.permission_drawer_content_len = 2;

        handle_mouse_event(&mut state, MouseEventKind::ScrollUp, 12, 3);

        assert_eq!(state.permission_scroll_offset, usize::MAX);
        assert!(state.scroll_offset > 0);
        assert!(!state.auto_scroll);

        let mut state = state_with_permission_pause();
        state.permission_drawer_area = ratatui::layout::Rect::new(0, 10, 80, 6);
        state.permission_drawer_body_area = ratatui::layout::Rect::new(3, 12, 74, 2);
        state.permission_drawer_content_len = 8;
        state.permission_scroll_offset = 0;

        handle_mouse_event(&mut state, MouseEventKind::ScrollUp, 12, 3);

        assert_eq!(state.permission_scroll_offset, 0);
        assert!(state.scroll_offset > 0);
        assert!(!state.auto_scroll);
    }

    #[tokio::test]
    async fn plan_approval_enter_sends_selected_action() {
        let mut state = UiState::new();
        state.apply_event(RuntimeToUiEvent::PlanSubmitted(submitted_plan()));
        state.plan_approval_selected = 1;
        let (tx, mut rx) = mpsc::channel(1);

        handle_plan_approval_key(&mut state, KeyCode::Enter, &tx).await;

        let Some(ClientRequest::PlanResolve { plan_id, action }) = rx.recv().await else {
            panic!("expected plan approval response");
        };
        assert_eq!(plan_id, "20260521T000000Z-plan");
        assert_eq!(
            action,
            omini_protocol::PlanApprovalAction::ApproveInNewSession {
                profile: omini_protocol::PlanExecutionProfile::Main,
            }
        );
        assert!(state.plan_approval.is_none());
        assert!(!state.plan_approval_auto);
    }

    #[tokio::test]
    async fn plan_approval_auto_toggle_sends_auto_profile() {
        let mut state = UiState::new();
        state.apply_event(RuntimeToUiEvent::PlanSubmitted(submitted_plan()));
        let (tx, mut rx) = mpsc::channel(1);

        handle_plan_approval_key(&mut state, KeyCode::Char('a'), &tx).await;
        assert!(state.plan_approval_auto);
        handle_plan_approval_key(&mut state, KeyCode::Char('1'), &tx).await;

        let Some(ClientRequest::PlanResolve { action, .. }) = rx.recv().await else {
            panic!("expected plan approval response");
        };
        assert_eq!(
            action,
            omini_protocol::PlanApprovalAction::Approve {
                profile: omini_protocol::PlanExecutionProfile::Auto,
            }
        );
        assert!(state.plan_approval.is_none());
        assert!(!state.plan_approval_auto);
    }

    #[tokio::test]
    async fn plan_approval_esc_continues_discussion() {
        let mut state = UiState::new();
        state.apply_event(RuntimeToUiEvent::PlanSubmitted(submitted_plan()));
        state.plan_approval_auto = true;
        let (tx, mut rx) = mpsc::channel(1);

        handle_plan_approval_key(&mut state, KeyCode::Esc, &tx).await;

        let Some(ClientRequest::PlanResolve { action, .. }) = rx.recv().await else {
            panic!("expected plan approval response");
        };
        assert_eq!(
            action,
            omini_protocol::PlanApprovalAction::ContinueDiscussing
        );
        assert!(state.plan_approval.is_none());
        assert!(!state.plan_approval_auto);
    }

    #[tokio::test]
    async fn shift_tab_sends_profile_toggle() {
        let mut state = UiState::new();
        let (tx, mut rx) = mpsc::channel(1);

        let handled =
            handle_key_event(&mut state, KeyCode::BackTab, KeyModifiers::SHIFT, &tx).await;

        assert!(handled);
        let Some(ClientRequest::ProfileToggle) = rx.recv().await else {
            panic!("expected profile toggle event");
        };
    }

    #[tokio::test]
    async fn auto_profile_permission_pause_uses_drawer_when_forwarded() {
        let mut state = UiState::new();
        state.status_bar.active_profile = ActiveProfile::Auto;
        let (tx, mut rx) = mpsc::channel(1);

        let outcome = handle_runtime_event(
            &mut state,
            RuntimeToUiEvent::ToolPauseRequested(permission_pause("tool_1")),
            &tx,
        )
        .await;

        assert!(outcome.redraw);
        assert!(state.active_tool_pause().is_some());
        assert_eq!(state.agent_status, AgentStatus::AwaitingInput);
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn repeat_key_events_edit_composer() {
        let mut state = UiState::new();
        let (tx, _rx) = mpsc::channel(1);

        handle_input_event(
            &mut state,
            Event::Key(KeyEvent::new_with_kind(
                KeyCode::Char('a'),
                KeyModifiers::NONE,
                KeyEventKind::Press,
            )),
            &tx,
        )
        .await;
        handle_input_event(
            &mut state,
            Event::Key(KeyEvent::new_with_kind(
                KeyCode::Char('a'),
                KeyModifiers::NONE,
                KeyEventKind::Repeat,
            )),
            &tx,
        )
        .await;
        handle_input_event(
            &mut state,
            Event::Key(KeyEvent::new_with_kind(
                KeyCode::Backspace,
                KeyModifiers::NONE,
                KeyEventKind::Repeat,
            )),
            &tx,
        )
        .await;

        assert_eq!(state.input, "a");
        assert_eq!(state.cursor_char, 1);
    }

    #[tokio::test]
    async fn compact_command_sets_manual_compact_working_state() {
        let mut state = UiState::new();
        state.input = "/compact".to_string();
        state.cursor_char = state.input.chars().count();
        let (tx, mut rx) = mpsc::channel(1);

        handle_composer_key(&mut state, KeyCode::Enter, KeyModifiers::NONE, &tx).await;

        assert_eq!(state.agent_status, AgentStatus::Working);
        assert!(state.manual_compact_running);
        assert!(state.run_timer.is_some());
        let Some(ClientRequest::ContextCompact { instructions }) = rx.recv().await else {
            panic!("expected compact command");
        };
        assert_eq!(instructions, None);
    }

    #[tokio::test]
    async fn command_submit_preserves_argument_mentions() {
        let mut state = UiState::new();
        state.input = "/commit-message summarize @src/main.rs".to_string();
        state.cursor_char = state.input.chars().count();
        state.input_mentions.push(InputMention {
            start_char: 26,
            end_char: 38,
            kind: MentionKind::File,
            label: "src/main.rs".to_string(),
            target: "src/main.rs".to_string(),
            description: "file".to_string(),
        });
        let (tx, mut rx) = mpsc::channel(1);

        handle_composer_key(&mut state, KeyCode::Enter, KeyModifiers::NONE, &tx).await;

        let Some(ClientRequest::ExpandSkillRun {
            input: Some(command),
            ..
        }) = rx.recv().await
        else {
            panic!("expected command draft");
        };
        assert_eq!(command.text, "/commit-message summarize @src/main.rs");
        let context_refs = command.context_refs.expect("expected context refs");
        assert_eq!(context_refs.len(), 1);
        assert_eq!(context_refs[0].target(), "src/main.rs");
    }

    #[tokio::test]
    async fn effort_command_accepts_max() {
        let mut state = UiState::new();
        state.input = "/effort max".to_string();
        state.cursor_char = state.input.chars().count();
        let (tx, mut rx) = mpsc::channel(1);

        handle_composer_key(&mut state, KeyCode::Enter, KeyModifiers::NONE, &tx).await;

        let Some(ClientRequest::ModelThinkingEffortSet { effort }) = rx.recv().await else {
            panic!("expected effort request");
        };
        assert_eq!(effort, omini_protocol::ThinkingEffort::Max);
    }

    #[tokio::test]
    async fn manual_compact_does_not_queue_normal_user_input() {
        let mut state = UiState::new();
        state.begin_manual_compact();
        state.input = "hello".to_string();
        state.cursor_char = state.input.chars().count();
        let (tx, mut rx) = mpsc::channel(1);

        handle_composer_key(&mut state, KeyCode::Enter, KeyModifiers::NONE, &tx).await;

        assert!(state.queued_user_inputs.is_empty());
        let Some(ClientRequest::RunSubmitUserInput { .. }) = rx.recv().await else {
            panic!("expected user message");
        };
    }
}
