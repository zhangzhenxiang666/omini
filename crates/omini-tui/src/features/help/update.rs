use crate::app::state::AppState;
use crossterm::event::{KeyCode, KeyModifiers};

pub fn handle_help_drawer_key(state: &mut AppState, code: KeyCode, modifiers: KeyModifiers) {
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
                .dialogs
                .help_drawer
                .as_ref()
                .map(|_| state.geometry.messages_area.height as usize / 2)
                .unwrap_or(1)
                .max(1);
            state.help_page_down(page);
        }
        (KeyCode::PageUp, _) => {
            let page = state
                .dialogs
                .help_drawer
                .as_ref()
                .map(|_| state.geometry.messages_area.height as usize / 2)
                .unwrap_or(1)
                .max(1);
            state.help_page_up(page);
        }
        _ => {}
    }
}
