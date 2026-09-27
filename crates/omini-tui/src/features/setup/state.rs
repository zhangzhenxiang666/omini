#[derive(Debug, Clone)]
pub struct ConfigurationForm {
    pub protocol: omini_protocol::ProviderEndpointKind,
    pub provider_id: String,
    pub base_url: String,
    pub model_id: String,
    pub environment_variable: String,
    pub api_key: String,
    pub selected: usize,
    pub cursor: usize,
    pub error: Option<String>,
}

impl ConfigurationForm {
    pub fn new(status: &omini_protocol::ProjectConfigurationResponse) -> Self {
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

    pub fn selected_value(&self) -> Option<&str> {
        match self.selected {
            1 => Some(&self.provider_id),
            2 => Some(&self.base_url),
            3 => Some(&self.model_id),
            4 => Some(&self.environment_variable),
            5 => Some(&self.api_key),
            _ => None,
        }
    }

    pub fn selected_value_mut(&mut self) -> Option<&mut String> {
        match self.selected {
            1 => Some(&mut self.provider_id),
            2 => Some(&mut self.base_url),
            3 => Some(&mut self.model_id),
            4 => Some(&mut self.environment_variable),
            5 => Some(&mut self.api_key),
            _ => None,
        }
    }

    pub fn select(&mut self, selected: usize) {
        self.selected = selected;
        self.cursor = self
            .selected_value()
            .map(|value| value.chars().count())
            .unwrap_or(0);
    }

    pub fn move_cursor_left(&mut self) {
        if self.selected_value().is_some() {
            self.cursor = self.cursor.saturating_sub(1);
        }
    }

    pub fn move_cursor_right(&mut self) {
        if let Some(value) = self.selected_value() {
            self.cursor = self.cursor.saturating_add(1).min(value.chars().count());
        }
    }

    pub fn move_cursor_to_start(&mut self) {
        if self.selected_value().is_some() {
            self.cursor = 0;
        }
    }

    pub fn move_cursor_to_end(&mut self) {
        if let Some(value) = self.selected_value() {
            self.cursor = value.chars().count();
        }
    }

    pub fn insert_text(&mut self, text: &str) {
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

    pub fn backspace(&mut self) {
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

    pub fn delete(&mut self) {
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

    pub fn validation_error(&self) -> Option<&'static str> {
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

pub fn char_to_byte_index(value: &str, char_index: usize) -> usize {
    value
        .char_indices()
        .nth(char_index)
        .map(|(byte, _)| byte)
        .unwrap_or(value.len())
}

pub fn default_environment_variable(provider_id: &str) -> String {
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

    pub fn form() -> ConfigurationForm {
        ConfigurationForm::new(&ProjectConfigurationResponse {
            state: ProjectConfigurationState::SetupRequired,
            code: Some("no_provider".to_string()),
            message: None,
            provider_id: None,
        })
    }

    #[test]
    pub fn text_cursor_supports_insertion_backspace_and_delete() {
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
    pub fn pasted_text_is_inserted_at_the_cursor_without_line_breaks() {
        let mut form = form();
        form.select(1);
        form.move_cursor_to_start();
        form.move_cursor_right();
        form.insert_text("MINI\n");

        assert_eq!(form.provider_id, "oMINIpenai");
        assert_eq!(form.cursor, 5);
    }

    #[test]
    pub fn text_cursor_uses_character_indices_for_unicode() {
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
