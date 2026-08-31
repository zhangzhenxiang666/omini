mod support;

use omini_config::{
    ConfigError, ModelSelection, OminiRoot, ProviderProtocol, RawConfig, RoutingTier,
    load_resolved_config_for_cwd,
};
use omini_domain::config::{InputModality, ThinkingEffort};
use std::path::Path;
use support::TestTempDir;

const USER_CONFIG: &str = r#"
[agent]
language = "简体中文"

[providers.openai]
protocol = "openai"
base_url = "https://openai.example/v1"
api_key = "user-key"

[providers.openai.request.headers]
x-provider = "user"

[providers.openai.request.body]
route = "provider"

[providers.openai.models.base]
context_window = 100000

thinking = true
input = ["text", "image"]

[providers.openai.models.base.request.headers]
x-model = "base"

[providers.openai.models.base.request.body]
route = "model"
"#;

fn full_configuration_example() -> &'static str {
    include_str!("../../../docs/configuration.md")
        .split_once("<!-- config-example:full:start -->\n\n```toml\n")
        .expect("full configuration example should start")
        .1
        .split_once("\n```\n\n<!-- config-example:full:end -->")
        .expect("full configuration example should end")
        .0
}

#[test]
fn full_documentation_example_parses() {
    let _: RawConfig = toml::from_str(full_configuration_example())
        .expect("full configuration example should parse");
}

#[test]
fn raw_config_merges_then_resolves_to_an_ordered_catalog() {
    let mut config: RawConfig = toml::from_str(USER_CONFIG).expect("user config should parse");
    let project: RawConfig = toml::from_str(
        r#"
[providers.openai]
base_url = "https://project.example/v1"

[providers.openai.models.project]
name = "Project model"
context_window = 32000

[routing]
small = { provider = "openai", model = "project" }
"#,
    )
    .expect("project config should parse");

    config
        .merge_project_config(project)
        .expect("project config should merge");
    let resolved = config.resolve().expect("merged config should resolve");

    assert_eq!(resolved.agent.language.as_deref(), Some("简体中文"));
    assert_eq!(resolved.providers.keys().collect::<Vec<_>>(), ["openai"]);
    assert_eq!(
        resolved.providers["openai"]
            .models
            .keys()
            .collect::<Vec<_>>(),
        ["base", "project"]
    );
    assert_eq!(
        resolved.providers["openai"].base_url.as_str(),
        "https://project.example/v1"
    );
    assert_eq!(
        resolved
            .routing
            .small
            .as_ref()
            .expect("small routing")
            .model,
        "project"
    );
}

#[test]
fn effective_model_config_merges_provider_then_model_overrides() {
    let config: RawConfig = toml::from_str(USER_CONFIG).expect("config should parse");
    let resolved = config.resolve().expect("config should resolve");
    let effective = resolved
        .effective_model_config(&ModelSelection {
            active_provider: "openai".into(),
            model: "base".into(),
            thinking_effort: None,
        })
        .expect("configured model should resolve");

    assert_eq!(effective.headers["x-provider"], "user");
    assert_eq!(effective.headers["x-model"], "base");
    assert_eq!(effective.body["route"], "model");
}

#[test]
fn settings_state_is_consistent() {
    let config: RawConfig = toml::from_str(&format!(
        "{USER_CONFIG}\n[providers.openai.models.plain]\nthinking = false\n"
    ))
    .expect("config should parse");
    let resolved = config.resolve().expect("config should resolve");
    let cwd = Path::new("/project/workspace");
    let mut settings = resolved
        .to_settings(Some("openai"), Some("base"), None, cwd)
        .expect("settings should build");

    // 运行目录由项目调用方明确传入，不能依赖后台进程自身的 cwd。
    assert_eq!(settings.cwd, cwd);

    // 当前模型是从已解析配置中生成的一份完整且规范化的快照。
    let active = settings.active_model();
    assert_eq!(active.provider_id, "openai");
    assert_eq!(active.model_id, "base");
    assert_eq!(active.protocol, ProviderProtocol::OpenAI);
    assert_eq!(active.base_url.as_str(), "https://openai.example/v1");
    assert_eq!(
        active.api_key.as_ref().map(|secret| secret.expose()),
        Some("user-key")
    );
    assert_eq!(active.context_window, 100_000);
    assert!(active.capabilities.thinking);
    assert!(active.capabilities.input.contains(&InputModality::Image));
    assert_eq!(active.headers["x-provider"], "user");
    assert_eq!(active.headers["x-model"], "base");
    assert_eq!(active.body["route"], "model");
    assert_eq!(active.thinking_effort, Some(ThinkingEffort::Medium));

    // 切换成功时整体替换快照，解析失败时保持原状态不变。
    settings
        .set_thinking_effort(Some(ThinkingEffort::High))
        .expect("thinking effort should update through model selection");
    assert_eq!(
        settings.active_model().thinking_effort,
        Some(ThinkingEffort::High)
    );
    settings
        .select_model(ModelSelection {
            active_provider: "openai".into(),
            model: "plain".into(),
            thinking_effort: Some(ThinkingEffort::High),
        })
        .expect("non-thinking model should normalize inherited effort");
    assert_eq!(settings.active_model().thinking_effort, None);

    let before = settings.active_model().clone();
    let error = settings
        .select_model(ModelSelection {
            active_provider: "missing".into(),
            model: "base".into(),
            thinking_effort: None,
        })
        .expect_err("invalid selection should fail");
    assert!(matches!(error, ConfigError::UnknownProvider(provider) if provider == "missing"));
    assert_eq!(settings.active_model().provider_id, before.provider_id);
    assert_eq!(settings.active_model().model_id, before.model_id);
    assert_eq!(
        settings.active_model().thinking_effort,
        before.thinking_effort
    );
}

#[test]
fn configured_routing_reference_must_exist() {
    let config: RawConfig = toml::from_str(
        r#"
[providers.openai]
protocol = "openai"
base_url = "https://openai.example"

[providers.openai.models.base]

[routing]
small = { provider = "openai", model = "missing" }
"#,
    )
    .expect("config should parse");

    assert!(matches!(
        config.resolve(),
        Err(ConfigError::UnknownRoutingModel { provider, model })
            if provider == "openai" && model == "missing"
    ));
}

#[test]
fn omitted_routing_slot_falls_back_to_current_selection() {
    let config: RawConfig = toml::from_str(USER_CONFIG).expect("config should parse");
    let resolved = config.resolve().expect("config should resolve");
    let current = resolved.first_selection();

    assert_eq!(
        resolved.routing_selection(RoutingTier::Small, &current),
        current
    );
}

#[test]
fn loader_reads_global_and_project_config_without_a_version_marker() {
    let temp = TestTempDir::new("configuration-loader");
    temp.write("config.toml", USER_CONFIG);
    let cwd = temp.create_dir("workspace");
    temp.write(
        "workspace/.omini/config.toml",
        r#"
[context.compaction]
enabled = false

[mcp.docs]
command = "docs"
"#,
    );
    let root = OminiRoot::from_path(temp.path().to_path_buf());
    let resolved = load_resolved_config_for_cwd(&root, &cwd).expect("config should load");

    assert!(!resolved.context.compaction.enabled);
    assert!(resolved.mcp.contains_key("docs"));
}

#[test]
fn missing_provider_fields_are_reported_after_merge() {
    let config: RawConfig = toml::from_str(
        r#"
[providers.openai]
base_url = "https://openai.example"

[providers.openai.models.base]
"#,
    )
    .expect("raw config should parse");

    assert!(matches!(
        config.resolve(),
        Err(ConfigError::MissingProviderField { provider, field })
            if provider == "openai" && field == "protocol"
    ));
}
