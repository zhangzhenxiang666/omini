use crate::client;
use crate::features::setup::state::ConfigurationForm;
use crate::features::setup::view;
use crate::platform::terminal;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use std::io;
pub async fn run(
    connection: client::ConfigurationConnection,
) -> io::Result<Option<client::ProjectConnection>> {
    if connection.status.state == omini_protocol::ProjectConfigurationState::Invalid {
        run_invalid(connection).await?;
        return Ok(None);
    }

    let _restore = terminal::RestoreGuard::new();
    let mut terminal = terminal::init()?;
    let mut form = ConfigurationForm::new(&connection.status);
    let project = loop {
        terminal.draw(|frame| view::render_form(frame, &form))?;
        let input = event::read()?;
        if let Event::Paste(text) = input {
            form.insert_text(&text);
            continue;
        }
        let Event::Key(key) = input else { continue };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match key.code {
            KeyCode::Esc => break None,
            KeyCode::Char('q') if form.selected == 6 => break None,
            KeyCode::Tab | KeyCode::Down => form.select((form.selected + 1) % 7),
            KeyCode::BackTab | KeyCode::Up => form.select((form.selected + 6) % 7),
            KeyCode::Left | KeyCode::Right | KeyCode::Char(' ') if form.selected == 0 => {
                form.error = None;
                form.protocol = match form.protocol {
                    omini_protocol::ProviderEndpointKind::OpenAI => {
                        omini_protocol::ProviderEndpointKind::Anthropic
                    }
                    omini_protocol::ProviderEndpointKind::Anthropic => {
                        omini_protocol::ProviderEndpointKind::OpenAI
                    }
                };
            }
            KeyCode::Left => form.move_cursor_left(),
            KeyCode::Right => form.move_cursor_right(),
            KeyCode::Home => form.move_cursor_to_start(),
            KeyCode::End => form.move_cursor_to_end(),
            KeyCode::Backspace => form.backspace(),
            KeyCode::Delete => form.delete(),
            KeyCode::Char(character) => form.insert_text(&character.to_string()),
            KeyCode::Enter if form.selected < 6 => form.select(form.selected + 1),
            KeyCode::Enter => {
                if let Some(error) = form.validation_error() {
                    form.error = Some(error.to_string());
                    continue;
                }
                match client::configuration::bootstrap_configuration(&connection, &form).await {
                    Ok(project) => break Some(project),
                    Err(error) => form.error = Some(error),
                }
            }
            _ => {}
        }
    };
    terminal::restore(&mut terminal)?;
    Ok(project)
}

async fn run_invalid(connection: client::ConfigurationConnection) -> io::Result<()> {
    let _restore = terminal::RestoreGuard::new();
    let mut terminal = terminal::init()?;
    loop {
        let message = connection
            .status
            .message
            .as_deref()
            .unwrap_or("The project configuration is invalid.");
        terminal.draw(|frame| view::render_invalid(frame, message))?;
        if let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
            && matches!(key.code, KeyCode::Esc | KeyCode::Char('q'))
        {
            break;
        }
    }
    terminal::restore(&mut terminal)
}
