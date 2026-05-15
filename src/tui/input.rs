use super::state::{AgentStatus, InteractionStep, ModelSelectionEntry, UiMessage, UiState};
use crate::types::events::{ToolPauseKind, ToolPauseResponse, UiToRuntimeEvent};
use crossterm::event::{KeyCode, KeyModifiers};
use tokio::sync::mpsc;

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
                KeyCode::Up | KeyCode::Char('k') => {
                    let mut new = selected.saturating_sub(1);
                    while new > 0 && matches!(&entries[new], E::ProviderHeader { .. }) {
                        new = new.saturating_sub(1);
                    }
                    if !matches!(&entries[new], E::ProviderHeader { .. }) {
                        *selected = new;
                    }
                    true
                }
                KeyCode::Down | KeyCode::Char('j') => {
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
                KeyCode::Left | KeyCode::Char('h') => {
                    if let E::Model { model, .. } = &entries[*selected]
                        && model.thinking
                    {
                        *thinking_idx = thinking_idx.saturating_sub(1);
                    }
                    true
                }
                KeyCode::Right | KeyCode::Char('l') => {
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
    let Some(msg) = state.take_queued_user_message() else {
        return;
    };

    state.messages.push(UiMessage::Message(msg.clone()));
    state.scroll_offset = 0;
    state.auto_scroll = true;
    state.agent_status = AgentStatus::Working;
    let _ = request_tx.send(UiToRuntimeEvent::SendMessage(msg)).await;
}

pub(super) async fn submit_queued_intervention(
    state: &mut UiState,
    request_tx: &mpsc::Sender<UiToRuntimeEvent>,
) {
    if state.is_run_active()
        && state.pending_intervention_inputs.is_empty()
        && let Some(msg) = state.take_queued_user_message_for_intervention()
    {
        let _ = request_tx
            .send(UiToRuntimeEvent::InterveneMessage(msg))
            .await;
    }
}

pub(super) fn is_intervention_key(code: KeyCode, modifiers: KeyModifiers) -> bool {
    modifiers.contains(KeyModifiers::ALT) && matches!(code, KeyCode::Enter)
}
