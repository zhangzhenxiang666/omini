mod support;

use omini_config::{ConfigError, OminiRoot, UserConfig};
use support::{MINIMAL_CONFIG, TestTempDir};

#[test]
fn explicit_root_derives_managed_paths() {
    let root = OminiRoot::from_path("/tmp/omini-root".into());

    assert_eq!(root.path(), &std::path::PathBuf::from("/tmp/omini-root"));
    assert_eq!(root.config_path(), root.path().join("config.toml"));
    assert_eq!(root.db_path(), root.path().join("omini.db"));
    assert_eq!(
        root.project_config_path(std::path::Path::new("/workspace/project")),
        std::path::PathBuf::from("/workspace/project/.omini/config.toml")
    );
    assert_eq!(root.projects_dir().path(), root.path().join("projects"));
}

#[test]
fn valid_user_config_loads() {
    let temp = TestTempDir::new("load-user");
    temp.write("config.toml", MINIMAL_CONFIG);
    let root = OminiRoot::from_path(temp.path().to_path_buf());

    let config = root.load_config().expect("user config should load");

    assert_eq!(config.providers.len(), 1);
    assert_eq!(config.providers["openai"].name.as_deref(), Some("OpenAI"));
    assert_eq!(
        config.providers["openai"]
            .models
            .as_ref()
            .expect("models should exist")["gpt-test"]
            .name
            .as_deref(),
        Some("GPT Test")
    );
    assert_eq!(config.language, None);
    assert!(config.mcp_servers.is_empty());
    assert_eq!(config.model_tiers, Default::default());
}

#[test]
fn missing_project_config_keeps_user_values() {
    let temp = TestTempDir::new("missing-project");
    temp.write("config.toml", MINIMAL_CONFIG);
    let cwd = temp.create_dir("workspace");
    let root = OminiRoot::from_path(temp.path().to_path_buf());

    let config = root
        .load_config_for_cwd(&cwd)
        .expect("missing project config should be optional");

    assert_eq!(
        config.providers["openai"].base_url,
        "https://openai.example"
    );
    assert_eq!(config.language, None);
}

#[test]
fn project_config_overrides_declared_fields() {
    let temp = TestTempDir::new("project-overlay");
    temp.write("config.toml", MINIMAL_CONFIG);
    let cwd = temp.create_dir("workspace");
    temp.write(
        "workspace/.omini/config.toml",
        r#"
language = "简体中文"

[providers.openai]
base_url = "https://project.example"
"#,
    );
    let root = OminiRoot::from_path(temp.path().to_path_buf());

    let config = root
        .load_config_for_cwd(&cwd)
        .expect("effective config should load");

    let provider = &config.providers["openai"];
    assert_eq!(config.language.as_deref(), Some("简体中文"));
    assert_eq!(provider.base_url, "https://project.example");
    assert_eq!(provider.api_key, "test-key");
    assert!(provider.models.as_ref().unwrap().contains_key("gpt-test"));
}

#[test]
fn missing_user_config_reports_path() {
    let temp = TestTempDir::new("missing-user");
    let root = OminiRoot::from_path(temp.path().to_path_buf());

    let error = root.load_config().expect_err("missing config should fail");

    assert!(matches!(
        error,
        ConfigError::ConfigLoad { path, source }
            if path == temp.path().join("config.toml")
                && source.kind() == std::io::ErrorKind::NotFound
    ));
}

#[test]
fn malformed_user_config_reports_path() {
    let temp = TestTempDir::new("invalid-user");
    temp.write("config.toml", "providers = [");
    let root = OminiRoot::from_path(temp.path().to_path_buf());

    let error = root
        .load_config()
        .expect_err("malformed config should fail");

    assert!(matches!(
        error,
        ConfigError::ConfigParse { path, .. }
            if path == temp.path().join("config.toml").display().to_string()
    ));
}

#[test]
fn malformed_project_config_reports_project_path() {
    let temp = TestTempDir::new("invalid-project");
    temp.write("config.toml", MINIMAL_CONFIG);
    let cwd = temp.create_dir("workspace");
    let project_config = temp.write("workspace/.omini/config.toml", "providers = [");
    let root = OminiRoot::from_path(temp.path().to_path_buf());

    let error = root
        .load_config_for_cwd(&cwd)
        .expect_err("malformed project config should fail");

    assert!(matches!(
        error,
        ConfigError::ConfigParse { path, .. } if path == project_config.display().to_string()
    ));
}

#[test]
fn validation_requires_provider_and_models() {
    let cases = [
        (
            "providers = {}",
            ConfigError::NoActiveProvider,
            "no provider should be rejected",
        ),
        (
            r#"
[providers.empty]
endpoint = "openai"
base_url = "https://empty.example"
api_key = "key"
"#,
            ConfigError::NoModels("empty".into()),
            "provider without models should be rejected",
        ),
        (
            r#"
[providers.empty]
endpoint = "openai"
base_url = "https://empty.example"
api_key = "key"
models = {}
"#,
            ConfigError::NoModels("empty".into()),
            "provider with an empty model map should be rejected",
        ),
    ];

    for (input, expected, context) in cases {
        let config: UserConfig = toml::from_str(input).expect("fixture should parse");
        let error = config.validate().expect_err(context);
        assert_same_validation_error(error, expected);
    }
}

#[test]
fn dotted_model_id_requires_quoted_key() {
    let error = toml::from_str::<UserConfig>(
        r#"
[providers.openai]
endpoint = "openai"
base_url = "https://openai.example"
api_key = "key"

[providers.openai.models.gpt-4.1]
name = "GPT 4.1"
"#,
    )
    .expect_err("unquoted dotted model id should be rejected");

    assert!(
        error.to_string().contains("unknown field"),
        "error should identify the nested shape as invalid: {error}"
    );
}

fn assert_same_validation_error(actual: ConfigError, expected: ConfigError) {
    match (actual, expected) {
        (ConfigError::NoActiveProvider, ConfigError::NoActiveProvider) => {}
        (ConfigError::NoModels(actual), ConfigError::NoModels(expected)) => {
            assert_eq!(actual, expected);
        }
        (actual, expected) => {
            panic!("unexpected validation error: {actual:?}, expected {expected:?}")
        }
    }
}
