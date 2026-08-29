mod view;

use crate::client;
use crate::terminal;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use std::io;
use std::time::Duration;

#[derive(Debug, Clone)]
struct ConfigurationForm {
    protocol: omini_protocol::ProviderEndpointKind,
    provider_id: String,
    base_url: String,
    model_id: String,
    environment_variable: String,
    api_key: String,
    selected: usize,
    cursor: usize,
    error: Option<String>,
}

impl ConfigurationForm {
    fn new(status: &omini_protocol::ProjectConfigurationResponse) -> Self {
        let provider_id = status
            .provider_id
            .clone()
            .unwrap_or_else(|| "openai".to_string());
        Self {
            protocol: omini_protocol::ProviderEndpointKind::OpenAI,
            environment_variable: default_environment_variable(&provider_id),
            provider_id,
            base_url: "https://api.openai.com/v1".to_string(),
            model_id: "gpt-5".to_string(),
            api_key: String::new(),
            selected: 0,
            cursor: 0,
            error: None,
        }
    }

    fn selected_value(&self) -> Option<&str> {
        match self.selected {
            1 => Some(&self.provider_id),
            2 => Some(&self.base_url),
            3 => Some(&self.model_id),
            4 => Some(&self.environment_variable),
            5 => Some(&self.api_key),
            _ => None,
        }
    }

    fn selected_value_mut(&mut self) -> Option<&mut String> {
        match self.selected {
            1 => Some(&mut self.provider_id),
            2 => Some(&mut self.base_url),
            3 => Some(&mut self.model_id),
            4 => Some(&mut self.environment_variable),
            5 => Some(&mut self.api_key),
            _ => None,
        }
    }

    fn select(&mut self, selected: usize) {
        self.selected = selected;
        self.cursor = self
            .selected_value()
            .map(|value| value.chars().count())
            .unwrap_or(0);
    }

    fn move_cursor_left(&mut self) {
        if self.selected_value().is_some() {
            self.cursor = self.cursor.saturating_sub(1);
        }
    }

    fn move_cursor_right(&mut self) {
        if let Some(value) = self.selected_value() {
            self.cursor = self.cursor.saturating_add(1).min(value.chars().count());
        }
    }

    fn move_cursor_to_start(&mut self) {
        if self.selected_value().is_some() {
            self.cursor = 0;
        }
    }

    fn move_cursor_to_end(&mut self) {
        if let Some(value) = self.selected_value() {
            self.cursor = value.chars().count();
        }
    }

    fn insert_text(&mut self, text: &str) {
        let text = text
            .chars()
            .filter(|character| !matches!(character, '\r' | '\n'))
            .collect::<String>();
        if text.is_empty() {
            return;
        }
        let cursor = self.cursor;
        let Some(value) = self.selected_value_mut() else {
            return;
        };
        let cursor = cursor.min(value.chars().count());
        let byte = char_to_byte_index(value, cursor);
        value.insert_str(byte, &text);
        self.cursor = cursor + text.chars().count();
        self.error = None;
    }

    fn backspace(&mut self) {
        let cursor = self.cursor;
        if cursor == 0 {
            return;
        }
        let Some(value) = self.selected_value_mut() else {
            return;
        };
        let cursor = cursor.min(value.chars().count());
        if cursor == 0 {
            return;
        }
        let start = char_to_byte_index(value, cursor - 1);
        let end = char_to_byte_index(value, cursor);
        value.replace_range(start..end, "");
        self.cursor = cursor - 1;
        self.error = None;
    }

    fn delete(&mut self) {
        let cursor = self.cursor;
        let Some(value) = self.selected_value_mut() else {
            return;
        };
        let len = value.chars().count();
        let cursor = cursor.min(len);
        if cursor == len {
            return;
        }
        let start = char_to_byte_index(value, cursor);
        let end = char_to_byte_index(value, cursor + 1);
        value.replace_range(start..end, "");
        self.cursor = cursor;
        self.error = None;
    }

    fn validation_error(&self) -> Option<&'static str> {
        if self.provider_id.trim().is_empty() {
            return Some("Provider ID is required.");
        }
        if self.base_url.trim().is_empty() {
            return Some("Base URL is required.");
        }
        if self.model_id.trim().is_empty() {
            return Some("Model ID is required.");
        }
        if !self.api_key.trim().is_empty() && self.environment_variable.trim().is_empty() {
            return Some("Environment variable is required when an API key is provided.");
        }
        None
    }
}

pub(crate) async fn run(
    connection: client::ConfigurationConnection,
) -> io::Result<Option<client::ProjectConnection>> {
    if connection.status.state == omini_protocol::ProjectConfigurationState::Invalid {
        run_invalid(connection).await?;
        return Ok(None);
    }

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
                match bootstrap_configuration(&connection, &form).await {
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

fn char_to_byte_index(value: &str, char_index: usize) -> usize {
    value
        .char_indices()
        .nth(char_index)
        .map(|(byte, _)| byte)
        .unwrap_or(value.len())
}

async fn run_invalid(connection: client::ConfigurationConnection) -> io::Result<()> {
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

async fn bootstrap_configuration(
    connection: &client::ConfigurationConnection,
    form: &ConfigurationForm,
) -> Result<client::ProjectConnection, String> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|error| format!("build configuration client: {error}"))?;
    let base = format!(
        "http://{}/v1/projects/{}",
        connection.addr, connection.project_id
    );
    let request = omini_protocol::BootstrapProjectConfigurationRequest {
        provider_id: form.provider_id.clone(),
        protocol: form.protocol,
        base_url: form.base_url.clone(),
        model_id: form.model_id.clone(),
        environment_variable: (!form.api_key.trim().is_empty())
            .then(|| form.environment_variable.clone()),
        api_key: (!form.api_key.trim().is_empty()).then(|| form.api_key.clone()),
    };
    let response = http
        .post(format!("{base}/configuration"))
        .json(&request)
        .send()
        .await
        .map_err(|error| format!("save configuration: {error}"))?;
    if !response.status().is_success() {
        return Err(response
            .text()
            .await
            .unwrap_or_else(|_| "save failed".to_string()));
    }
    let status: omini_protocol::ProjectConfigurationResponse = response
        .json()
        .await
        .map_err(|error| format!("read configuration result: {error}"))?;
    if status.state != omini_protocol::ProjectConfigurationState::Ready {
        return Err(status
            .message
            .unwrap_or_else(|| "configuration is still incomplete".to_string()));
    }
    let open = http
        .post(format!("{base}/open"))
        .send()
        .await
        .map_err(|error| format!("open configured project: {error}"))?;
    if !open.status().is_success() {
        return Err(open
            .text()
            .await
            .unwrap_or_else(|_| "project open failed".to_string()));
    }
    let open = open
        .json()
        .await
        .map_err(|error| format!("read configured project: {error}"))?;
    Ok(client::ProjectConnection {
        addr: connection.addr,
        project_id: connection.project_id.clone(),
        client_id: connection.client_id.clone(),
        open,
    })
}

fn default_environment_variable(provider_id: &str) -> String {
    let normalized = provider_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    format!("{normalized}_API_KEY")
}

#[cfg(test)]
mod tests {
    use super::*;
    use omini_protocol::{ProjectConfigurationResponse, ProjectConfigurationState};

    fn form() -> ConfigurationForm {
        ConfigurationForm::new(&ProjectConfigurationResponse {
            state: ProjectConfigurationState::SetupRequired,
            code: Some("no_provider".to_string()),
            message: None,
            provider_id: None,
        })
    }

    #[test]
    fn text_cursor_supports_insertion_backspace_and_delete() {
        let mut form = form();
        form.select(3);
        form.move_cursor_left();
        form.move_cursor_left();
        form.insert_text("X");
        assert_eq!(form.model_id, "gptX-5");
        assert_eq!(form.cursor, 4);

        form.backspace();
        assert_eq!(form.model_id, "gpt-5");
        assert_eq!(form.cursor, 3);

        form.delete();
        assert_eq!(form.model_id, "gpt5");
        assert_eq!(form.cursor, 3);
    }

    #[test]
    fn pasted_text_is_inserted_at_the_cursor_without_line_breaks() {
        let mut form = form();
        form.select(1);
        form.move_cursor_to_start();
        form.move_cursor_right();
        form.insert_text("MINI\n");

        assert_eq!(form.provider_id, "oMINIpenai");
        assert_eq!(form.cursor, 5);
    }

    #[test]
    fn text_cursor_uses_character_indices_for_unicode() {
        let mut form = form();
        form.select(1);
        form.provider_id = "模型a".to_string();
        form.move_cursor_to_end();
        form.move_cursor_left();
        form.backspace();

        assert_eq!(form.provider_id, "模a");
        assert_eq!(form.cursor, 1);
    }
}
