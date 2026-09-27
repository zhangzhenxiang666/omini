use crate::app::effect::Effects;
use crate::app::state::{AppState, UiMessage};
use crate::client::ClientRequest;
use crate::client::input as protocol;
use crossterm::event::{KeyCode, KeyModifiers};

pub fn flush_queued_user_inputs(state: &mut AppState, request_tx: &mut Effects) {
    let ui_messages = state
        .composer
        .queued_user_inputs
        .iter()
        .map(|draft| {
            UiMessage::SystemEvent(
                crate::features::timeline::model::UiSystemEvent::UserInputEcho(draft.clone()),
            )
        })
        .collect::<Vec<_>>();
    let Some(draft) = state.take_queued_user_draft() else {
        return;
    };

    state.clear_run_dividers();
    let client_echo_id = uuid::Uuid::new_v4().to_string();
    state.extend_optimistic_echoes(ui_messages, client_echo_id.clone());
    state.sessions.views["main"].scroll_offset = 0;
    state.sessions.views["main"].auto_scroll = true;
    state.begin_main_query_submission();
    let _ = request_tx.send(ClientRequest::RunSubmitUserInput {
        input: protocol::user_input_from_draft(draft),
        client_echo_id: Some(client_echo_id),
    });
}

pub fn submit_queued_intervention(state: &mut AppState, request_tx: &mut Effects) {
    if !state.is_main_query_active() {
        return;
    }

    let Some(draft) = state.take_queued_user_draft() else {
        return;
    };

    let _ = request_tx.send(ClientRequest::RunInterveneInput {
        input: protocol::user_input_from_draft(draft),
        client_echo_id: Some(uuid::Uuid::new_v4().to_string()),
    });
}

pub fn is_intervention_key(code: KeyCode, modifiers: KeyModifiers) -> bool {
    modifiers.contains(KeyModifiers::ALT) && matches!(code, KeyCode::Enter)
}

pub fn is_newline_key(code: KeyCode, modifiers: KeyModifiers) -> bool {
    matches!(code, KeyCode::Enter) && modifiers.contains(KeyModifiers::SHIFT)
        || matches!(code, KeyCode::Char('\n'))
        || matches!(code, KeyCode::Char('j')) && modifiers == KeyModifiers::CONTROL
}
