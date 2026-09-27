use crate::app::effect::Effects;
use crate::app::event::{ActiveProfile, CommandKind, RuntimeToUiEvent};
use crate::app::state::{AppState, UiMessage};
use crate::client::ClientRequest;
use crate::client::input as protocol;
use crate::features::agents::actions as input;
use crate::features::timeline::model::UserDraft;
use crossterm::event::{KeyCode, KeyModifiers};
use omini_domain::task::TaskStatus;

pub fn handle_command_autocomplete_key(
    state: &mut AppState,
    code: KeyCode,
    modifiers: KeyModifiers,
    request_tx: &mut Effects,
) {
    if input::is_newline_key(code, modifiers) {
        state.composer.insert_text("\n");
        state.composer.update_input_autocomplete();
        return;
    }

    match code {
        KeyCode::Enter | KeyCode::Tab => {
            if let Some(cmd) = state.composer.autocomplete.selected_command().cloned() {
                if cmd.has_args {
                    state.composer.input = format!("/{} ", cmd.name);
                    state.composer.input_mentions.clear();
                    state.composer.input_paste_markers.clear();
                    state.composer.input_scroll_line = 0;
                    state.composer.cursor_char = state.composer.input.chars().count();
                    state.composer.autocomplete.visible = false;
                } else {
                    state.composer.autocomplete.visible = false;
                    state.composer.input = format!("/{}", cmd.name);
                    state.composer.input_mentions.clear();
                    state.composer.input_paste_markers.clear();
                    state.composer.input_scroll_line = 0;
                    let msg = std::mem::take(&mut state.composer.input);
                    state.composer.cursor_char = 0;
                    if !msg.is_empty() {
                        let draft = crate::features::timeline::model::UserDraft::plain(msg);
                        if let Some(request) = request_from_command_draft(state, draft) {
                            let _ = request_tx.send(request);
                        }
                    }
                }
            }
            state.composer.autocomplete.visible = false;
        }
        KeyCode::Down => state.composer.autocomplete.select_next(),
        KeyCode::Up => state.composer.autocomplete.select_prev(),
        KeyCode::Esc => state.composer.autocomplete.visible = false,
        KeyCode::Backspace => {
            state.composer.delete_before();
            state.composer.update_input_autocomplete();
        }
        KeyCode::Delete => {
            state.composer.delete_after();
            state.composer.update_input_autocomplete();
        }
        KeyCode::Char(c) => {
            state.composer.insert_char(c);
            state.composer.update_input_autocomplete();
        }
        KeyCode::Left => {
            state.composer.cursor_left();
            state.composer.update_input_autocomplete();
        }
        KeyCode::Right => {
            state.composer.cursor_right();
            state.composer.update_input_autocomplete();
        }
        KeyCode::Home => {
            state.composer.cursor_home();
            state.composer.update_input_autocomplete();
        }
        KeyCode::End => {
            state.composer.cursor_end();
            state.composer.update_input_autocomplete();
        }
        _ => {}
    }
}

pub fn handle_mention_autocomplete_key(
    state: &mut AppState,
    code: KeyCode,
    modifiers: KeyModifiers,
) {
    if input::is_newline_key(code, modifiers) {
        state.composer.insert_text("\n");
        state.composer.update_input_autocomplete();
        return;
    }

    match code {
        KeyCode::Enter => {
            state.composer.insert_selected_mention();
            state.composer.update_input_autocomplete();
        }
        KeyCode::Tab | KeyCode::Right => {
            state.composer.expand_selected_mention_directory();
            state.composer.update_input_autocomplete();
        }
        KeyCode::Down => state.composer.mention_autocomplete.select_next(),
        KeyCode::Up => state.composer.mention_autocomplete.select_prev(),
        KeyCode::Esc => state.composer.cancel_mention_autocomplete(),
        KeyCode::Backspace => {
            state.composer.delete_before();
            state.composer.update_input_autocomplete();
        }
        KeyCode::Delete => {
            state.composer.delete_after();
            state.composer.update_input_autocomplete();
        }
        KeyCode::Char(c) => {
            state.composer.insert_char(c);
            state.composer.update_input_autocomplete();
        }
        KeyCode::Left => {
            state.composer.cursor_left();
            state.composer.update_input_autocomplete();
        }
        KeyCode::Home => {
            state.composer.cursor_home();
            state.composer.update_input_autocomplete();
        }
        KeyCode::End => {
            state.composer.cursor_end();
            state.composer.update_input_autocomplete();
        }
        _ => {}
    }
}

pub fn is_compact_command(text: &str) -> bool {
    let Some(rest) = text.trim().strip_prefix('/') else {
        return false;
    };
    rest.split_whitespace().next() == Some("compact")
}

pub fn parse_slash_command(text: &str) -> Option<(String, String)> {
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

pub fn request_from_command_draft(state: &mut AppState, draft: UserDraft) -> Option<ClientRequest> {
    let Some((name, args)) = parse_slash_command(&draft.text) else {
        return Some(ClientRequest::RunSubmitUserInput {
            input: protocol::user_input_from_draft(draft),
            client_echo_id: None,
        });
    };

    match name.as_str() {
        "exit" | "quit" => Some(ClientRequest::AppShutdown),
        "help" | "?" => {
            state.open_help_drawer(state.composer.autocomplete.all_commands.clone());
            None
        }
        "model" => Some(ClientRequest::OpenModelPicker),
        "sessions" | "resume" => Some(ClientRequest::OpenThreadPicker),
        "agents" => Some(ClientRequest::OpenAgentManager),
        "new" | "clear" => Some(ClientRequest::ThreadNew {
            profile: protocol::active_profile_from_internal(
                state.project.status_bar.active_profile,
            ),
        }),
        "plan" => Some(ClientRequest::ProfileSet {
            profile: protocol::active_profile_from_internal(ActiveProfile::Plan),
        }),
        "compact" => Some(ClientRequest::ContextCompact {
            instructions: (!args.is_empty()).then_some(args),
        }),
        "rename" => Some(ClientRequest::ThreadRename { title: args }),
        "init" => Some(ClientRequest::RunExecuteCommand {
            command: omini_protocol::RunCommand::Init,
            input: protocol::command_input_from_draft(draft, "init"),
            client_echo_id: None,
        }),
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
        skill_name => {
            let is_known_skill = state.composer.autocomplete.all_commands.iter().any(|cmd| {
                cmd.kind == CommandKind::Skill
                    && (cmd.name == skill_name || cmd.aliases.iter().any(|a| a == skill_name))
            });
            if is_known_skill {
                Some(ClientRequest::RunSubmitUserInput {
                    input: protocol::skill_input_from_draft(draft, skill_name.to_string()),
                    client_echo_id: None,
                })
            } else {
                Some(ClientRequest::RunSubmitUserInput {
                    input: protocol::user_input_from_draft(draft),
                    client_echo_id: None,
                })
            }
        }
    }
}

pub fn handle_composer_key(
    state: &mut AppState,
    code: KeyCode,
    modifiers: KeyModifiers,
    request_tx: &mut Effects,
) -> bool {
    let page_amt = 1.max(state.geometry.messages_area.height as usize / 2);
    if state.sessions.session_selector_focused {
        match code {
            KeyCode::Up => {
                if state.sessions.session_selection_index == 0 {
                    state.sessions.session_selector_focused = false;
                } else {
                    state.sessions.session_selection_index -= 1;
                }
            }
            KeyCode::Down => {
                state.sessions.session_selection_index = (state.sessions.session_selection_index
                    + 1)
                .min(state.session_count().saturating_sub(1));
            }
            KeyCode::Enter => {
                state.sessions.active_session_task_id =
                    if state.sessions.session_selection_index == 0 {
                        None
                    } else {
                        state
                            .sessions
                            .subagent_order
                            .get(state.sessions.session_selection_index - 1)
                            .cloned()
                    };
                state.sessions.session_selector_focused = false;
                state.prune_terminal_tasks();
            }
            _ => {}
        }
        return true;
    }
    if state.session_is_terminal()
        && !state.composer.input.starts_with('/')
        && !(state.composer.input.is_empty()
            && matches!((code, modifiers), (KeyCode::Char('/'), KeyModifiers::NONE)))
        && !matches!(
            code,
            KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown
        )
    {
        return true;
    }
    match (code, modifiers) {
        (KeyCode::Char('c'), KeyModifiers::CONTROL) | (KeyCode::Char('\x03'), _) => {
            return state.composer.clear_input();
        }
        (KeyCode::Up, _) => {
            state.composer.cursor_up_in_input();
        }
        (KeyCode::Down, _) => {
            if !state.composer.cursor_down_in_input() && state.session_count() > 1 {
                state.sessions.session_selector_focused = true;
                state.sessions.session_selection_index = state
                    .sessions
                    .active_session_task_id
                    .as_ref()
                    .and_then(|task_id| {
                        state
                            .sessions
                            .subagent_order
                            .iter()
                            .position(|id| id == task_id)
                    })
                    .map(|index| index + 1)
                    .unwrap_or(0);
            }
        }
        (KeyCode::PageUp, _) => {
            state.update_scroll_step(tokio::time::Instant::now());
            state.scroll_up(state.selection.scroll_step.max(page_amt));
        }
        (KeyCode::PageDown, _) => {
            state.update_scroll_step(tokio::time::Instant::now());
            state.scroll_down(state.selection.scroll_step.max(page_amt));
        }
        (code, modifiers) if input::is_intervention_key(code, modifiers) => {
            input::submit_queued_intervention(state, request_tx);
        }
        (code, modifiers) if input::is_newline_key(code, modifiers) => {
            state.composer.insert_text("\n");
            state.composer.update_input_autocomplete();
        }
        (KeyCode::Enter, _) => {
            if let Some(draft) = state.composer.take_input_draft() {
                if draft.text.starts_with('/') {
                    if !state.is_run_active() && is_compact_command(&draft.text) {
                        state.begin_manual_compact();
                    }
                    if let Some(request) = request_from_command_draft(state, draft) {
                        match (state.sessions.active_session_task_id.clone(), request) {
                            (Some(task_id), ClientRequest::RunSubmitUserInput { input, .. }) => {
                                let is_active = state.sessions.subagents.values().any(|node| {
                                    node.task_id == task_id
                                        && matches!(
                                            node.status,
                                            TaskStatus::Running | TaskStatus::Cancelling
                                        )
                                });
                                if is_active {
                                    let client_echo_id = uuid::Uuid::new_v4().to_string();
                                    let _ = request_tx.send(ClientRequest::AgentTaskSubmitInput {
                                        task_id,
                                        input,
                                        client_echo_id: Some(client_echo_id),
                                    });
                                }
                            }
                            (_, request) => {
                                let _ = request_tx.send(request);
                            }
                        }
                    }
                } else if let Some(task_id) = state.sessions.active_session_task_id.clone() {
                    let is_active = state.sessions.subagents.values().any(|node| {
                        node.task_id == task_id
                            && matches!(node.status, TaskStatus::Running | TaskStatus::Cancelling)
                    });
                    if !is_active {
                        return true;
                    }
                    let client_echo_id = uuid::Uuid::new_v4().to_string();
                    let _ = request_tx.send(ClientRequest::AgentTaskSubmitInput {
                        task_id,
                        input: protocol::user_input_from_draft(draft),
                        client_echo_id: Some(client_echo_id),
                    });
                } else if state.is_main_query_active()
                    && !state.sessions.views["main"].manual_compact_running
                {
                    state.composer.queued_user_inputs.push_back(draft);
                } else {
                    state.clear_run_dividers();
                    state.start.show_start_screen = false;
                    let input = protocol::user_input_from_draft(draft.clone());
                    let ui_message = UiMessage::SystemEvent(
                        crate::features::timeline::model::UiSystemEvent::UserInputEcho(draft),
                    );
                    let client_echo_id = uuid::Uuid::new_v4().to_string();
                    state.push_optimistic_echo(ui_message, client_echo_id.clone());
                    let _ = request_tx.send(ClientRequest::RunSubmitUserInput {
                        input,
                        client_echo_id: Some(client_echo_id),
                    });
                    state.sessions.views["main"].scroll_offset = 0;
                    state.sessions.views["main"].auto_scroll = true;
                    state.begin_main_query_submission();
                }
            }
        }
        (KeyCode::Backspace, _) => {
            state.composer.delete_before();
            state.composer.update_input_autocomplete();
        }
        (KeyCode::Delete, _) => {
            state.composer.delete_after();
            state.composer.update_input_autocomplete();
        }
        (KeyCode::Char(c), _) => {
            state.composer.insert_char(c);
            state.composer.update_input_autocomplete();
        }
        (KeyCode::Left, _) => {
            state.composer.cursor_left();
            state.composer.update_input_autocomplete();
        }
        (KeyCode::Right, _) => {
            state.composer.cursor_right();
            state.composer.update_input_autocomplete();
        }
        (KeyCode::Home, KeyModifiers::CONTROL) => state.scroll_to_top(),
        (KeyCode::End, KeyModifiers::CONTROL) => state.scroll_to_bottom(),
        (KeyCode::Home, _) => {
            state.composer.cursor_home();
            state.composer.update_input_autocomplete();
        }
        (KeyCode::End, _) => {
            state.composer.cursor_end();
            state.composer.update_input_autocomplete();
        }
        _ => {}
    }
    true
}
