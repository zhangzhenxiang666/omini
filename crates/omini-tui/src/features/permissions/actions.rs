use crate::app::effect::Effects;
use crate::app::event::{ToolPauseKind, ToolPauseResponse};
use crate::app::state::AppState;
use crate::client::ClientRequest;
use crate::client::input as protocol;

pub fn resolve_active_tool_pause(state: &mut AppState, request_tx: &mut Effects) {
    let Some(req) = state.active_tool_pause().cloned() else {
        return;
    };

    let response = match &req.kind {
        ToolPauseKind::Permission(_) => {
            let approved = state.dialogs.permission.permission_selected == 0;
            let note = (!approved)
                .then(|| state.current_user_input_note().trim())
                .filter(|note| !note.is_empty())
                .map(str::to_string);
            ToolPauseResponse::Permission { approved, note }
        }
        ToolPauseKind::UserInput(preview) => {
            let mut answers = serde_json::Map::new();

            for (idx, question) in preview.questions.iter().enumerate() {
                let custom_idx = question.options.len();
                let selected = state
                    .dialogs
                    .ask
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
                    .dialogs
                    .ask
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

    let _ = request_tx.send(ClientRequest::ToolPauseResolve {
        tool_use_id: req.tool_use_id.clone(),
        response: protocol::tool_pause_response_from_internal(response),
    });
    let removed_active = state.remove_tool_pause(&req.tool_use_id);
    state.finish_tool_pause_removal(removed_active);
}
