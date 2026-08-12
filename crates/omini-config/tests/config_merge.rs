use omini_config::{ConfigError, McpServerTransportConfig, PartialUserConfig, UserConfig};
use omini_domain::config::{InputModality, ProviderEndpointKind, ThinkingEffort};

fn user_config(input: &str) -> UserConfig {
    toml::from_str(input).expect("user config fixture should parse")
}

fn project_config(input: &str) -> PartialUserConfig {
    toml::from_str(input).expect("project config fixture should parse")
}

fn minimal_user_config() -> UserConfig {
    user_config(
        r#"
[providers.openai]
endpoint = "openai"
base_url = "https://user.example"
api_key = "user-key"

[providers.openai.models.base]
"#,
    )
}

#[test]
fn scalar_overlay_preserves_omitted_fields() {
    let mut config = user_config(
        r#"
language = "English"

[providers.openai]
endpoint = "openai"
base_url = "https://user.example"
api_key = "user-key"

[providers.openai.models.base]

[permissions]
allow = ["Read"]
ask = ["Edit"]
deny = ["Delete"]

[compact]
enabled = true
preserve_recent = 8
buffer_tokens = 9000
summary_output_tokens = 12000
max_consecutive_failures = 5
"#,
    );

    config
        .merge_project_config(project_config(
            r#"
language = "  简体中文  "

[permissions]
allow = []
deny = ["Write"]

[compact]
enabled = false
preserve_recent = 0
"#,
        ))
        .expect("overlay should merge");

    assert_eq!(config.language.as_deref(), Some("  简体中文  "));
    let permissions = config
        .permissions
        .expect("permissions should remain present");
    assert!(permissions.allow.is_empty());
    assert_eq!(permissions.ask, ["Edit"]);
    assert_eq!(permissions.deny, ["Write"]);
    let compact = config.compact.expect("compact should remain present");
    assert!(!compact.enabled);
    assert_eq!(compact.preserve_recent, 0);
    assert_eq!(compact.buffer_tokens, 9000);
    assert_eq!(compact.summary_output_tokens, 12000);
    assert_eq!(compact.max_consecutive_failures, 5);
}

#[test]
fn provider_overlay_merges_deeply() {
    let mut config = user_config(
        r#"
[providers.openai]
name = "User Provider"
endpoint = "openai"
base_url = "https://user.example"
api_key = "user-key"

[providers.openai.models.fast]
name = "User Fast"
limit = 1000
thinking = false
input_modalities = ["text"]

[providers.openai.models.fast.headers]
x-user = "keep-out"

[providers.openai.models.fast.body]
route = "user"

[providers.openai.models.legacy]
name = "Legacy"
limit = 2000
"#,
    );

    // provider/model map 按 key 深合并，但 headers/body 是单个字段，项目值会整体替换用户值。
    config
        .merge_project_config(project_config(
            r#"
[providers.openai]
name = "Project Provider"
endpoint = "anthropic"
base_url = "https://project.example"
api_key = "project-key"

[providers.openai.models.fast]
name = "Project Fast"
limit = 0
thinking = true
input_modalities = []

[providers.openai.models.fast.headers]
x-project = "replacement"

[providers.openai.models.fast.body]
route = "project"
nested = { enabled = true }

[providers.openai.models.reasoner]
thinking = true
input_modalities = ["text", "image"]
"#,
        ))
        .expect("provider overlay should merge");

    let provider = &config.providers["openai"];
    assert_eq!(provider.name.as_deref(), Some("Project Provider"));
    assert_eq!(provider.endpoint, ProviderEndpointKind::Anthropic);
    assert_eq!(provider.base_url, "https://project.example");
    assert_eq!(provider.api_key, "project-key");
    let models = provider.models.as_ref().expect("models should exist");
    assert_eq!(models.len(), 3);

    let fast = &models["fast"];
    assert_eq!(fast.name.as_deref(), Some("Project Fast"));
    assert_eq!(fast.limit, Some(0));
    assert_eq!(fast.thinking, Some(true));
    assert_eq!(fast.input_modalities.as_deref(), Some([].as_slice()));
    assert_eq!(
        fast.headers.as_ref().expect("headers should exist"),
        &std::collections::HashMap::from([("x-project".into(), "replacement".into())])
    );
    assert_eq!(
        fast.body.as_ref().expect("body should exist"),
        &serde_json::Map::from_iter([
            ("route".into(), serde_json::json!("project")),
            ("nested".into(), serde_json::json!({"enabled": true})),
        ])
    );

    assert_eq!(models["legacy"].name.as_deref(), Some("Legacy"));
    assert_eq!(models["legacy"].limit, Some(2000));
    assert_eq!(models["reasoner"].thinking, Some(true));
    assert_eq!(
        models["reasoner"].input_modalities.as_deref(),
        Some([InputModality::Text, InputModality::Image].as_slice())
    );
}

#[test]
fn complete_project_provider_is_accepted() {
    let mut config = minimal_user_config();

    config
        .merge_project_config(project_config(
            r#"
[providers.anthropic]
name = "Anthropic"
endpoint = "anthropic"
base_url = "https://anthropic.example"
api_key = "anthropic-key"

[providers.anthropic.models.claude]
limit = 200000
thinking = true
"#,
        ))
        .expect("complete provider should be added");

    config.validate().expect("merged config should be valid");
    assert_eq!(config.providers.len(), 2);
    let provider = &config.providers["anthropic"];
    assert_eq!(provider.name.as_deref(), Some("Anthropic"));
    assert_eq!(provider.endpoint, ProviderEndpointKind::Anthropic);
    assert_eq!(provider.base_url, "https://anthropic.example");
    assert_eq!(provider.api_key, "anthropic-key");
    assert_eq!(
        provider.models.as_ref().unwrap()["claude"].limit,
        Some(200000)
    );
}

#[test]
fn incomplete_project_provider_is_not_inserted() {
    // 新 provider 只有在必需字段全部就绪后才可插入，错误不能留下半成品。
    let cases = [
        (
            r#"
[providers.anthropic]
base_url = "https://anthropic.example"
api_key = "key"
"#,
            "endpoint",
        ),
        (
            r#"
[providers.anthropic]
endpoint = "anthropic"
api_key = "key"
"#,
            "base_url",
        ),
        (
            r#"
[providers.anthropic]
endpoint = "anthropic"
base_url = "https://anthropic.example"
"#,
            "api_key",
        ),
    ];

    for (input, expected_field) in cases {
        let mut config = minimal_user_config();
        let error = config
            .merge_project_config(project_config(input))
            .expect_err("incomplete new provider should fail");

        assert!(matches!(
            error,
            ConfigError::ProjectProviderFieldRequired { provider, field }
                if provider == "anthropic" && field == expected_field
        ));
        assert_eq!(config.providers.len(), 1);
        assert!(!config.providers.contains_key("anthropic"));
    }
}

#[test]
fn project_mcp_override_is_atomic() {
    let mut config = user_config(
        r#"
[providers.openai]
endpoint = "openai"
base_url = "https://user.example"
api_key = "key"

[providers.openai.models.base]

[mcp_servers.docs]
command = "user-docs"
args = ["--user"]
tool_timeout_sec = 30.0

[mcp_servers.remote]
url = "https://remote.example/mcp"
"#,
    );

    // MCP transport 不做字段级拼接，避免用户级 args 与项目级 command 组成意外配置。
    config
        .merge_project_config(project_config(
            r#"
[mcp_servers.docs]
command = "project-docs"
enabled = false
"#,
        ))
        .expect("MCP overlay should merge");

    assert_eq!(config.mcp_servers.len(), 2);
    let docs = &config.mcp_servers["docs"];
    assert!(!docs.enabled);
    assert_eq!(docs.tool_timeout_sec, None);
    assert_eq!(
        docs.transport,
        McpServerTransportConfig::Stdio {
            command: "project-docs".into(),
            args: Vec::new(),
            env: None,
            cwd: None,
        }
    );
    assert!(matches!(
        config.mcp_servers["remote"].transport,
        McpServerTransportConfig::StreamableHttp { .. }
    ));
}

#[test]
fn tier_overlay_replaces_only_present_slots() {
    let mut config = user_config(
        r#"
[providers.openai]
endpoint = "openai"
base_url = "https://user.example"
api_key = "key"

[providers.openai.models]
fast = {}
reasoner = { thinking = true }

[model_tiers.small]
provider = "openai"
model = "fast"

[model_tiers.standard]
provider = "openai"
model = "reasoner"
thinking_effort = "low"

[model_tiers.large]
provider = "openai"
model = "reasoner"
thinking_effort = "max"
"#,
    );
    let original_standard = config.model_tiers.standard.clone();
    let original_large = config.model_tiers.large.clone();

    config
        .merge_project_config(project_config(
            r#"
[model_tiers.small]
provider = "openai"
model = "reasoner"
thinking_effort = "high"
"#,
        ))
        .expect("tier overlay should merge");

    assert_eq!(
        config.model_tiers.small,
        Some(omini_config::ModelTierEntry {
            provider: "openai".into(),
            model: "reasoner".into(),
            thinking_effort: Some(ThinkingEffort::High),
        })
    );
    assert_eq!(config.model_tiers.standard, original_standard);
    assert_eq!(config.model_tiers.large, original_large);
}

#[test]
fn repeated_overlay_is_idempotent() {
    let overlay = r#"
language = "中文"

[permissions]
allow = ["Read"]

[providers.openai]
base_url = "https://project.example"

[providers.openai.models.extra]
thinking = true

[mcp_servers.docs]
command = "docs"
"#;
    let mut config = minimal_user_config();

    // 配置可能在项目重载时重复合并，同一 overlay 不应累积列表或复制 map 项。
    config
        .merge_project_config(project_config(overlay))
        .expect("first merge should succeed");
    config
        .merge_project_config(project_config(overlay))
        .expect("second merge should succeed");

    assert_eq!(config.language.as_deref(), Some("中文"));
    assert_eq!(config.providers.len(), 1);
    assert_eq!(
        config.providers["openai"].base_url,
        "https://project.example"
    );
    assert_eq!(config.providers["openai"].models.as_ref().unwrap().len(), 2);
    assert_eq!(config.permissions.unwrap().allow, ["Read"]);
    assert_eq!(config.mcp_servers.len(), 1);
    assert_eq!(
        config.mcp_servers["docs"].transport,
        McpServerTransportConfig::Stdio {
            command: "docs".into(),
            args: Vec::new(),
            env: None,
            cwd: None,
        }
    );
}

#[test]
fn unknown_model_fields_are_rejected() {
    let user_error = toml::from_str::<UserConfig>(
        r#"
[providers.openai]
endpoint = "openai"
base_url = "https://example.invalid"
api_key = "key"

[providers.openai.models.base]
unknown = true
"#,
    )
    .expect_err("unknown user model field should fail");
    let project_error = toml::from_str::<PartialUserConfig>(
        r#"
[providers.openai.models.base]
unknown = true
"#,
    )
    .expect_err("unknown project model field should fail");

    assert!(user_error.to_string().contains("unknown"));
    assert!(project_error.to_string().contains("unknown"));
}
