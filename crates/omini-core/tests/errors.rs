use omini_config::ConfigError;
use omini_core::CoreError;
use std::error::Error as _;

#[test]
fn core_errors_expose_stable_codes_messages_and_sources() {
    let config = CoreError::config("load settings", ConfigError::NoActiveProvider);
    assert_eq!(config.code(), "config_error");
    assert_eq!(config.message(), "load settings: no providers configured");
    assert_eq!(
        config.source().map(ToString::to_string),
        Some("no providers configured".into())
    );

    let invalid = CoreError::invalid_model_selection("unsupported model");
    assert_eq!(invalid.code(), "invalid_model_selection");
    assert_eq!(invalid.message(), "unsupported model");
    assert!(invalid.source().is_none());
}
