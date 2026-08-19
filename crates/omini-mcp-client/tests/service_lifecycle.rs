use omini_mcp_client::{
    McpClientSet, McpServerConfig, McpServerTransportConfig, McpServiceStatus, McpServiceSummary,
};
use serde_json::{Map, json};
use std::collections::HashMap;
use std::path::PathBuf;

fn stdio_config(enabled: bool) -> McpServerConfig {
    McpServerConfig {
        transport: McpServerTransportConfig::Stdio {
            command: "/omini-test/does-not-exist".into(),
            args: Vec::new(),
            env: None,
            cwd: None,
        },
        enabled,
        startup_timeout_sec: None,
        tool_timeout_sec: None,
        enabled_tools: None,
        disabled_tools: None,
    }
}

fn client_set(servers: Vec<(&str, McpServerConfig)>) -> McpClientSet {
    McpClientSet::new(
        PathBuf::from("/omini-test/fallback-cwd"),
        servers
            .into_iter()
            .map(|(name, config)| (name.to_string(), config)),
    )
}

#[test]
fn empty_set_has_empty_views() {
    let clients = client_set(Vec::new());

    assert!(clients.is_empty());
    assert_eq!(clients.services(), Vec::new());
    assert_eq!(clients.status(), Vec::new());
    assert!(clients.snapshots().is_empty());
    assert!(clients.ready_server_tools().is_empty());
    assert_eq!(clients.service_status("missing"), None);
    assert!(clients.catalog("missing").is_none());
}

#[test]
fn initial_state_is_sorted() {
    let clients = client_set(vec![
        ("zeta", stdio_config(true)),
        ("alpha", stdio_config(false)),
    ]);
    let expected = vec![
        McpServiceSummary {
            name: "alpha".into(),
            status: McpServiceStatus::Disabled,
            last_error: None,
        },
        McpServiceSummary {
            name: "zeta".into(),
            status: McpServiceStatus::Connecting,
            last_error: None,
        },
    ];

    assert!(!clients.is_empty());
    assert_eq!(clients.services(), expected);
    assert_eq!(clients.status(), expected);

    let snapshots = clients.snapshots();
    assert_eq!(
        snapshots
            .iter()
            .map(|snapshot| (
                snapshot.name.as_str(),
                snapshot.status,
                snapshot.last_error.as_deref(),
            ))
            .collect::<Vec<_>>(),
        vec![
            ("alpha", McpServiceStatus::Disabled, None),
            ("zeta", McpServiceStatus::Connecting, None),
        ]
    );
    assert!(snapshots.iter().all(|snapshot| {
        snapshot.catalog.tools.is_empty()
            && snapshot.catalog.resources.is_empty()
            && snapshot.catalog.resource_templates.is_empty()
            && snapshot.catalog.prompts.is_empty()
    }));
}

#[tokio::test]
async fn disabled_servers_stay_disabled() {
    let clients = client_set(vec![
        ("beta", stdio_config(false)),
        ("alpha", stdio_config(false)),
    ]);

    assert_eq!(clients.initialize().await, Vec::<String>::new());
    assert_eq!(
        clients.services(),
        vec![
            McpServiceSummary {
                name: "alpha".into(),
                status: McpServiceStatus::Disabled,
                last_error: None,
            },
            McpServiceSummary {
                name: "beta".into(),
                status: McpServiceStatus::Disabled,
                last_error: None,
            },
        ]
    );
}

#[tokio::test]
async fn startup_failures_are_isolated() {
    let mut zero = stdio_config(true);
    zero.startup_timeout_sec = Some(0.0);
    let mut infinite = stdio_config(true);
    infinite.tool_timeout_sec = Some(f64::INFINITY);
    let clients = client_set(vec![
        ("zero", zero),
        ("disabled", stdio_config(false)),
        ("infinite", infinite),
    ]);

    let warnings = clients.initialize().await;

    assert_eq!(
        warnings,
        vec![
            "MCP server `infinite` failed to start: MCP timeout must be a positive finite number, got inf",
            "MCP server `zero` failed to start: MCP timeout must be a positive finite number, got 0",
        ]
    );
    assert_eq!(
        clients.services(),
        vec![
            McpServiceSummary {
                name: "disabled".into(),
                status: McpServiceStatus::Disabled,
                last_error: None,
            },
            McpServiceSummary {
                name: "infinite".into(),
                status: McpServiceStatus::Failed,
                last_error: Some("MCP timeout must be a positive finite number, got inf".into(),),
            },
            McpServiceSummary {
                name: "zero".into(),
                status: McpServiceStatus::Failed,
                last_error: Some("MCP timeout must be a positive finite number, got 0".into()),
            },
        ]
    );
    assert!(clients.ready_server_tools().is_empty());
    assert!(
        clients
            .snapshots()
            .iter()
            .all(|snapshot| snapshot.catalog.tools.is_empty())
    );
}

#[tokio::test]
async fn initialization_is_cached() {
    let mut config = stdio_config(true);
    config.startup_timeout_sec = Some(-1.0);
    let clients = client_set(vec![("broken", config)]);

    let first = clients.initialize().await;
    let first_status = clients.services();
    let second = clients.initialize().await;

    assert_eq!(
        first,
        vec![
            "MCP server `broken` failed to start: MCP timeout must be a positive finite number, got -1"
                .to_string()
        ]
    );
    assert_eq!(second, first);
    assert_eq!(clients.services(), first_status);
    assert_eq!(
        clients.service_status("broken"),
        Some(McpServiceStatus::Failed)
    );
}

#[tokio::test]
async fn unready_calls_are_rejected() {
    let clients = client_set(vec![
        ("disabled", stdio_config(false)),
        ("connecting", stdio_config(true)),
    ]);

    assert_eq!(
        clients
            .call_tool("missing", "search", Map::new())
            .await
            .expect_err("unknown server must be rejected"),
        "Unknown MCP server: missing"
    );
    assert_eq!(
        clients
            .call_tool("disabled", "search", Map::new())
            .await
            .expect_err("disabled server must be rejected"),
        "MCP server `disabled` is not ready: Disabled"
    );
    assert_eq!(
        clients
            .read_resource("connecting", "file:///doc")
            .await
            .expect_err("connecting server must be rejected"),
        "MCP server `connecting` is not ready: Connecting"
    );
    assert_eq!(
        clients
            .get_prompt(
                "connecting",
                "explain",
                Some(Map::from_iter([("tone".into(), json!("short"))]))
            )
            .await
            .expect_err("connecting server must be rejected"),
        "MCP server `connecting` is not ready: Connecting"
    );
}

#[tokio::test]
async fn stdio_spawn_failure_is_retained() {
    let clients = client_set(vec![
        ("missing-command", stdio_config(true)),
        ("offline", stdio_config(false)),
    ]);

    let warnings = clients.initialize().await;
    let services = clients.services();
    let failure = services[0]
        .last_error
        .as_deref()
        .expect("failed service should retain the cause");

    // OS 文本可能变化，只锁定客户端提供的错误类别，并用状态和 warning 验证完整传播链。
    assert!(failure.starts_with("failed to spawn stdio MCP command `/omini-test/does-not-exist`:"));
    assert_eq!(
        warnings,
        vec![format!(
            "MCP server `missing-command` failed to start: {failure}"
        )]
    );
    assert_eq!(
        services
            .iter()
            .map(|service| (service.name.as_str(), service.status))
            .collect::<Vec<_>>(),
        vec![
            ("missing-command", McpServiceStatus::Failed),
            ("offline", McpServiceStatus::Disabled),
        ]
    );
}

#[tokio::test]
async fn http_preflight_rejects_invalid_inputs() {
    let missing_token = McpServerConfig {
        transport: McpServerTransportConfig::StreamableHttp {
            url: "http://127.0.0.1:1/mcp".into(),
            bearer_token_env_var: Some(String::new()),
            http_headers: None,
        },
        enabled: true,
        startup_timeout_sec: None,
        tool_timeout_sec: None,
        enabled_tools: None,
        disabled_tools: None,
    };
    let invalid_header = McpServerConfig {
        transport: McpServerTransportConfig::StreamableHttp {
            url: "http://127.0.0.1:1/mcp".into(),
            bearer_token_env_var: None,
            http_headers: Some(HashMap::from([("bad header".into(), "value".into())])),
        },
        ..missing_token.clone()
    };
    let clients = client_set(vec![
        ("missing-token", missing_token),
        ("invalid-header", invalid_header),
    ]);

    let warnings = clients.initialize().await;
    let services = clients.services();

    assert_eq!(warnings.len(), 2);
    assert_eq!(services[0].name, "invalid-header");
    assert_eq!(services[0].status, McpServiceStatus::Failed);
    assert!(
        services[0]
            .last_error
            .as_deref()
            .expect("invalid header should have a cause")
            .starts_with("invalid MCP HTTP header name `bad header`:")
    );
    assert_eq!(
        services[1],
        McpServiceSummary {
            name: "missing-token".into(),
            status: McpServiceStatus::Failed,
            last_error: Some(
                "environment variable  for MCP server `missing-token` is not set".into(),
            ),
        }
    );
    for (warning, service) in warnings.iter().zip(services.iter()) {
        let cause = service
            .last_error
            .as_deref()
            .expect("failed service should retain its preflight error");
        assert_eq!(
            warning,
            &format!("MCP server `{}` failed to start: {}", service.name, cause)
        );
    }
}
