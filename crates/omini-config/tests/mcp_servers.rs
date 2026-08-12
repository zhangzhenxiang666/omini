use omini_config::{McpServerConfig, McpServerTransportConfig, UserConfig};
use std::collections::HashMap;
use std::path::PathBuf;

fn mcp_config(input: &str) -> McpServerConfig {
    toml::from_str(input).expect("MCP config fixture should parse")
}

fn toml_value(input: &str) -> toml::Value {
    toml::from_str(input).expect("expected TOML fixture should parse")
}

#[test]
fn minimal_stdio_uses_defaults_and_compact_shape() {
    let config = mcp_config(r#"command = "server""#);

    assert_eq!(
        config,
        McpServerConfig {
            transport: McpServerTransportConfig::Stdio {
                command: "server".into(),
                args: Vec::new(),
                env: None,
                cwd: None,
            },
            enabled: true,
            startup_timeout_sec: None,
            tool_timeout_sec: None,
            enabled_tools: None,
            disabled_tools: None,
        }
    );
    assert_eq!(
        toml::Value::try_from(&config).expect("MCP config should serialize"),
        toml_value(
            r#"
command = "server"
enabled = true
"#
        )
    );
}

#[test]
fn stdio_round_trip_preserves_all_fields() {
    let input = r#"
command = "server"
args = ["--stdio", "终"]
env = { TOKEN = "secret", MODE = "test" }
cwd = "/workspace/project"
enabled = false
startup_timeout_sec = 1.5
tool_timeout_sec = 30.25
enabled_tools = ["search", "read"]
disabled_tools = []
"#;
    let config = mcp_config(input);

    assert_eq!(
        config,
        McpServerConfig {
            transport: McpServerTransportConfig::Stdio {
                command: "server".into(),
                args: vec!["--stdio".into(), "终".into()],
                env: Some(HashMap::from([
                    ("TOKEN".into(), "secret".into()),
                    ("MODE".into(), "test".into()),
                ])),
                cwd: Some(PathBuf::from("/workspace/project")),
            },
            enabled: false,
            startup_timeout_sec: Some(1.5),
            tool_timeout_sec: Some(30.25),
            enabled_tools: Some(vec!["search".into(), "read".into()]),
            disabled_tools: Some(Vec::new()),
        }
    );
    assert_eq!(
        toml::Value::try_from(&config).expect("MCP config should serialize"),
        toml_value(input)
    );
}

#[test]
fn http_round_trip_preserves_auth_fields() {
    let input = r#"
url = "https://mcp.example/service"
bearer_token_env_var = "MCP_TOKEN"
http_headers = { X_Feature = "enabled", X_Unicode = "中文" }
enabled = true
startup_timeout_sec = 0.0
disabled_tools = ["delete"]
"#;
    let config = mcp_config(input);

    assert_eq!(
        config,
        McpServerConfig {
            transport: McpServerTransportConfig::StreamableHttp {
                url: "https://mcp.example/service".into(),
                bearer_token_env_var: Some("MCP_TOKEN".into()),
                http_headers: Some(HashMap::from([
                    ("X_Feature".into(), "enabled".into()),
                    ("X_Unicode".into(), "中文".into()),
                ])),
            },
            enabled: true,
            startup_timeout_sec: Some(0.0),
            tool_timeout_sec: None,
            enabled_tools: None,
            disabled_tools: Some(vec!["delete".into()]),
        }
    );
    assert_eq!(
        toml::Value::try_from(&config).expect("MCP config should serialize"),
        toml_value(input)
    );
}

#[test]
fn transport_fields_are_mutually_exclusive() {
    // 两种 transport 共享一个扁平表，必须穷举互斥字段，防止 serde 静默吞掉错配配置。
    let cases = [
        (
            r#"command = "local"
url = "https://remote.example""#,
            "either command or url, not both",
        ),
        ("enabled = true", "must set command or url"),
        (
            r#"command = "local"
bearer_token_env_var = "TOKEN""#,
            "bearer_token_env_var is not supported for stdio",
        ),
        (
            r#"command = "local"
http_headers = { X = "y" }"#,
            "http_headers is not supported for stdio",
        ),
        (
            r#"url = "https://remote.example"
args = []"#,
            "args is not supported for streamable HTTP",
        ),
        (
            r#"url = "https://remote.example"
env = {}"#,
            "env is not supported for streamable HTTP",
        ),
        (
            r#"url = "https://remote.example"
cwd = "/tmp""#,
            "cwd is not supported for streamable HTTP",
        ),
    ];

    for (input, reason) in cases {
        let error = toml::from_str::<McpServerConfig>(input)
            .expect_err("invalid transport combination should fail");
        assert!(
            error.to_string().contains(reason),
            "error should explain {reason:?}: {error}"
        );
    }
}

#[test]
fn unknown_mcp_fields_are_rejected() {
    let error = toml::from_str::<McpServerConfig>(
        r#"
command = "server"
unknown_option = true
"#,
    )
    .expect_err("unknown MCP field should fail");

    assert!(error.to_string().contains("unknown_option"));
}

#[test]
fn mcp_config_reaches_runtime_unchanged() {
    let config: UserConfig = toml::from_str(
        r#"
[providers.openai]
endpoint = "openai"
base_url = "https://openai.example"
api_key = "key"

[providers.openai.models.base]

[mcp_servers.local]
command = "local-server"
args = ["--stdio"]

[mcp_servers.remote]
url = "https://mcp.example"
enabled = false
"#,
    )
    .expect("user config should parse");
    let expected = config.mcp_servers.clone();

    let settings = config
        .to_settings(Some("openai"), Some("base"), None)
        .expect("settings should build");

    assert_eq!(settings.mcp_servers, expected);
}
