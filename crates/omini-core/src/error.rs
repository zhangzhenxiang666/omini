use crate::types::config::ConfigError;
use omini_provider_api::{RequestError, StreamError};
use std::borrow::Cow;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("{message}")]
    Internal { message: String },
    #[error("runtime session is closed")]
    RuntimeClosed,
    #[error("runtime session loading was interrupted")]
    RuntimeLoadInterrupted,
    #[error("session does not exist")]
    SessionNotFound,
    #[error("{message}")]
    InvalidModelSelection { message: String },
    #[error("{context}: {source}")]
    Config {
        context: &'static str,
        #[source]
        source: Box<ConfigError>,
    },
    #[error("{context}: {source}")]
    ProjectState {
        context: &'static str,
        #[source]
        source: Box<ConfigError>,
    },
    #[error("{context}: {message}")]
    Persistence {
        context: &'static str,
        message: String,
    },
    #[error("failed to encode runtime event: {source}")]
    RuntimeEventEncode {
        #[source]
        source: Box<serde_json::Error>,
    },
    #[error("{message}")]
    Subagent { message: String },
}

impl CoreError {
    pub fn new(message: impl Into<String>) -> Self {
        Self::Internal {
            message: message.into(),
        }
    }

    pub fn invalid_model_selection(message: impl Into<String>) -> Self {
        Self::InvalidModelSelection {
            message: message.into(),
        }
    }

    pub fn config(context: &'static str, source: ConfigError) -> Self {
        Self::Config {
            context,
            source: Box::new(source),
        }
    }

    pub fn project_state(context: &'static str, source: ConfigError) -> Self {
        Self::ProjectState {
            context,
            source: Box::new(source),
        }
    }

    pub fn persistence(context: &'static str, message: impl Into<String>) -> Self {
        Self::Persistence {
            context,
            message: message.into(),
        }
    }

    pub fn subagent(message: impl Into<String>) -> Self {
        Self::Subagent {
            message: message.into(),
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::Internal { .. } => "core_error",
            Self::RuntimeClosed => "runtime_closed",
            Self::RuntimeLoadInterrupted => "runtime_load_interrupted",
            Self::SessionNotFound => "session_not_found",
            Self::InvalidModelSelection { .. } => "invalid_model_selection",
            Self::Config { .. } => "config_error",
            Self::ProjectState { .. } => "project_state_error",
            Self::Persistence { .. } => "persistence_error",
            Self::RuntimeEventEncode { .. } => "runtime_event_encode_error",
            Self::Subagent { .. } => "subagent_error",
        }
    }

    pub fn message(&self) -> Cow<'_, str> {
        match self {
            Self::Internal { message }
            | Self::InvalidModelSelection { message }
            | Self::Subagent { message } => Cow::Borrowed(message),
            _ => Cow::Owned(self.to_string()),
        }
    }
}

#[derive(Debug, Error)]
pub(crate) enum RuntimeError {
    #[error("LLM request failed: {0}")]
    ProviderRequest(#[source] RequestError),
    #[error("Stream error: {0}")]
    ProviderStream(#[source] StreamError),
    #[error("{0}")]
    Compact(#[from] CompactError),
    #[error("tool pause waiter closed: {tool_use_id}")]
    ToolPauseWaiterClosed { tool_use_id: String },
    #[error("tool pause response type mismatch: {tool_use_id}")]
    ToolPauseResponseTypeMismatch { tool_use_id: String },
    #[error("{message}")]
    InvalidRequest { message: String },
}

#[derive(Debug, Error)]
pub(crate) enum CompactError {
    #[error("compact summary request failed: {0}")]
    Request(#[source] RequestError),
    #[error("compact summary stream failed: {0}")]
    Stream(#[source] StreamError),
    #[error("Compaction interrupted before a complete summary was returned.")]
    IncompleteResponse,
}

#[cfg(test)]
mod tests {
    use crate::error::CoreError;
    use crate::types::config::ConfigError;
    use std::error::Error as _;

    #[test]
    fn core_config_error_preserves_source() {
        let error = CoreError::config("failed to load user config", ConfigError::NoActiveProvider);

        assert_eq!(error.code(), "config_error");
        assert_eq!(
            error.source().map(ToString::to_string),
            Some("no providers configured".to_string())
        );
    }
}
