use crate::tools::{Tool, ToolRegistry, ToolResult, tool_metadata};
use async_trait::async_trait;
use omini_config::{
    McpServerConfig as CoreMcpServerConfig,
    McpServerTransportConfig as CoreMcpServerTransportConfig, Settings,
};
use omini_domain::events::{McpPermissionPreview, PermissionPreview};
use omini_mcp_client::{
    GetPromptResult, McpCallOutput, McpCatalog, McpClientSet,
    McpServerConfig as ClientMcpServerConfig, McpServerToolSpec,
    McpServerTransportConfig as ClientMcpServerTransportConfig, McpServiceSnapshot,
    McpServiceStatus, McpServiceSummary, ReadResourceResult,
};
use omini_runtime_contract::mcp::{
    RuntimeMcpServerSnapshot, RuntimeMcpServerStatus, RuntimeMcpToolSnapshot,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

const MCP_TOOL_PREFIX: &str = "mcp";
const MCP_TOOL_SEPARATOR: &str = "__";
const MAX_TOOL_NAME_LEN: usize = 64;

pub struct McpManager {
    client_set: McpClientSet,
}

#[derive(Debug, Clone)]
struct RegisteredMcpToolSpec {
    /// 本地注册并暴露给 provider/LLM 的工具名。
    registered_tool_name: String,
    server_name: String,
    /// MCP server 原始工具名，调用 server 时使用。
    server_tool_name: String,
    description: String,
    input_schema: Value,
}

#[derive(Clone)]
struct McpRuntimeTool {
    manager: Arc<McpManager>,
    spec: RegisteredMcpToolSpec,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
struct McpToolInput {
    #[serde(flatten)]
    arguments: HashMap<String, Value>,
}

#[derive(Debug, Clone)]
struct PreparedMcpToolInput {
    server_name: String,
    server_tool_name: String,
    arguments: Map<String, Value>,
}

impl McpManager {
    pub fn from_settings(settings: &Settings) -> Self {
        let servers = settings
            .mcp_servers()
            .iter()
            .map(|(name, config)| (name.clone(), client_config_from_core(config)));

        Self {
            client_set: McpClientSet::new(settings.cwd.clone(), servers),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.client_set.is_empty()
    }

    pub async fn initialize(&self) -> Vec<String> {
        self.client_set.initialize().await
    }

    pub fn register_available_tools(self: &Arc<Self>, registry: &mut ToolRegistry) {
        let server_tools = self.client_set.ready_server_tools();
        register_server_tools(self, registry, server_tools);
    }

    async fn call_tool(
        &self,
        server_name: &str,
        server_tool_name: &str,
        arguments: Map<String, Value>,
    ) -> Result<McpCallOutput, String> {
        self.client_set
            .call_tool(server_name, server_tool_name, arguments)
            .await
    }

    #[allow(dead_code)]
    pub async fn read_resource(
        &self,
        server_name: &str,
        uri: &str,
    ) -> Result<ReadResourceResult, String> {
        self.client_set.read_resource(server_name, uri).await
    }

    #[allow(dead_code)]
    pub async fn get_prompt(
        &self,
        server_name: &str,
        name: &str,
        arguments: Option<Map<String, Value>>,
    ) -> Result<GetPromptResult, String> {
        self.client_set
            .get_prompt(server_name, name, arguments)
            .await
    }

    #[allow(dead_code)]
    pub fn services(&self) -> Vec<McpServiceSummary> {
        self.client_set.services()
    }

    #[allow(dead_code)]
    pub fn status(&self) -> Vec<McpServiceSummary> {
        self.client_set.status()
    }

    pub fn runtime_snapshots(&self) -> Vec<RuntimeMcpServerSnapshot> {
        runtime_snapshots_from_service_snapshots(self.client_set.snapshots())
    }

    #[allow(dead_code)]
    pub fn service_status(&self, server_name: &str) -> Option<McpServiceStatus> {
        self.client_set.service_status(server_name)
    }

    #[allow(dead_code)]
    pub fn catalog(&self, server_name: &str) -> Option<McpCatalog> {
        self.client_set.catalog(server_name)
    }
}

#[async_trait]
impl Tool for McpRuntimeTool {
    type Input = McpToolInput;
    type Prepared = PreparedMcpToolInput;

    fn name(&self) -> &str {
        &self.spec.registered_tool_name
    }

    fn description(&self) -> &str {
        &self.spec.description
    }

    fn input_schema(&self) -> Value {
        self.spec.input_schema.clone()
    }

    async fn prepare(&self, input: Self::Input) -> Result<Self::Prepared, ToolResult> {
        Ok(PreparedMcpToolInput {
            server_name: self.spec.server_name.clone(),
            server_tool_name: self.spec.server_tool_name.clone(),
            arguments: input.arguments.into_iter().collect(),
        })
    }

    fn permission_preview(&self, prepared: &Self::Prepared) -> Option<PermissionPreview> {
        Some(PermissionPreview::Mcp(McpPermissionPreview {
            server_name: prepared.server_name.clone(),
            server_tool_name: prepared.server_tool_name.clone(),
            registered_tool_name: self.spec.registered_tool_name.clone(),
            inputs: prepared.arguments.clone(),
        }))
    }

    async fn execute_prepared(
        &self,
        prepared: Self::Prepared,
        _ctx: crate::tools::ToolExecutionContext,
    ) -> ToolResult {
        let metadata = mcp_tool_metadata(
            &self.spec.server_name,
            &self.spec.server_tool_name,
            &self.spec.registered_tool_name,
        );
        match self
            .manager
            .call_tool(
                &prepared.server_name,
                &prepared.server_tool_name,
                prepared.arguments,
            )
            .await
        {
            Ok(output) if output.is_error => {
                ToolResult::error(output.content).with_metadata(metadata)
            }
            Ok(output) => ToolResult::ok(output.content).with_metadata(metadata),
            Err(error) => ToolResult::error(error).with_metadata(metadata),
        }
    }
}

fn client_config_from_core(config: &CoreMcpServerConfig) -> ClientMcpServerConfig {
    let transport = match &config.transport {
        CoreMcpServerTransportConfig::Stdio {
            command,
            args,
            env,
            cwd,
        } => ClientMcpServerTransportConfig::Stdio {
            command: command.clone(),
            args: args.clone(),
            env: env.clone(),
            cwd: cwd.clone(),
        },
        CoreMcpServerTransportConfig::StreamableHttp {
            url,
            bearer_token_env_var,
            http_headers,
        } => ClientMcpServerTransportConfig::StreamableHttp {
            url: url.clone(),
            bearer_token_env_var: bearer_token_env_var.clone(),
            http_headers: http_headers.clone(),
        },
    };

    ClientMcpServerConfig {
        transport,
        enabled: config.enabled,
        startup_timeout_sec: config.startup_timeout_sec,
        tool_timeout_sec: config.tool_timeout_sec,
        enabled_tools: config.enabled_tools.clone(),
        disabled_tools: config.disabled_tools.clone(),
    }
}

fn register_server_tools(
    manager: &Arc<McpManager>,
    registry: &mut ToolRegistry,
    server_tools: Vec<McpServerToolSpec>,
) {
    for spec in assign_registered_tool_names(server_tools) {
        registry.register(McpRuntimeTool {
            manager: Arc::clone(manager),
            spec,
        });
    }
}

fn mcp_tool_metadata(
    server_name: &str,
    server_tool_name: &str,
    registered_tool_name: &str,
) -> Map<String, Value> {
    tool_metadata([
        ("kind", serde_json::json!("mcp_tool")),
        ("server_name", serde_json::json!(server_name)),
        ("server_tool_name", serde_json::json!(server_tool_name)),
        (
            "registered_tool_name",
            serde_json::json!(registered_tool_name),
        ),
    ])
}

fn assign_registered_tool_names(
    server_tools: Vec<McpServerToolSpec>,
) -> Vec<RegisteredMcpToolSpec> {
    let mut server_tools = server_tools;
    server_tools.sort_by(|left, right| {
        left.server_name
            .cmp(&right.server_name)
            .then_with(|| left.server_tool_name.cmp(&right.server_tool_name))
    });

    let mut used = HashSet::new();
    let mut counts: HashMap<String, usize> = HashMap::new();
    server_tools
        .into_iter()
        .map(|server_tool| {
            let base =
                base_registered_tool_name(&server_tool.server_name, &server_tool.server_tool_name);
            let mut index = *counts.get(&base).unwrap_or(&0);
            let registered_tool_name = loop {
                let candidate = if index == 0 {
                    base.clone()
                } else {
                    suffixed_tool_name(&base, index)
                };
                index += 1;
                if used.insert(candidate.clone()) {
                    counts.insert(base.clone(), index);
                    break candidate;
                }
            };
            RegisteredMcpToolSpec {
                registered_tool_name,
                server_name: server_tool.server_name,
                server_tool_name: server_tool.server_tool_name,
                description: server_tool.description,
                input_schema: server_tool.input_schema,
            }
        })
        .collect()
}

fn runtime_snapshots_from_service_snapshots(
    snapshots: Vec<McpServiceSnapshot>,
) -> Vec<RuntimeMcpServerSnapshot> {
    let registered_tools = assign_registered_tool_names(
        snapshots
            .iter()
            .filter(|service| service.status == McpServiceStatus::Ready)
            .flat_map(|service| service.catalog.tools.clone())
            .collect(),
    )
    .into_iter()
    .map(|tool| {
        (
            (tool.server_name.clone(), tool.server_tool_name.clone()),
            tool.registered_tool_name,
        )
    })
    .collect::<HashMap<_, _>>();

    let mut runtime_snapshots = snapshots
        .into_iter()
        .map(|service| {
            let mut tools = service
                .catalog
                .tools
                .iter()
                .map(|tool| RuntimeMcpToolSnapshot {
                    name: tool.server_tool_name.clone(),
                    registered_name: registered_tools
                        .get(&(tool.server_name.clone(), tool.server_tool_name.clone()))
                        .cloned()
                        .unwrap_or_else(|| {
                            base_registered_tool_name(&tool.server_name, &tool.server_tool_name)
                        }),
                    description: tool.description.clone(),
                })
                .collect::<Vec<_>>();
            tools.sort_by(|left, right| left.name.cmp(&right.name));

            RuntimeMcpServerSnapshot {
                name: service.name,
                status: mcp_service_status_to_runtime(service.status),
                last_error: service.last_error,
                tools,
            }
        })
        .collect::<Vec<_>>();
    runtime_snapshots.sort_by(|left, right| left.name.cmp(&right.name));
    runtime_snapshots
}

fn mcp_service_status_to_runtime(status: McpServiceStatus) -> RuntimeMcpServerStatus {
    match status {
        McpServiceStatus::Disabled => RuntimeMcpServerStatus::Disabled,
        McpServiceStatus::Connecting => RuntimeMcpServerStatus::Connecting,
        McpServiceStatus::Ready => RuntimeMcpServerStatus::Ready,
        McpServiceStatus::Failed => RuntimeMcpServerStatus::Failed,
    }
}

fn base_registered_tool_name(server_name: &str, server_tool_name: &str) -> String {
    let raw = format!(
        "{MCP_TOOL_PREFIX}{MCP_TOOL_SEPARATOR}{server_name}{MCP_TOOL_SEPARATOR}{server_tool_name}"
    );
    truncate_tool_name(&sanitize_tool_name(&raw))
}

fn suffixed_tool_name(base: &str, index: usize) -> String {
    let suffix = format!("{MCP_TOOL_SEPARATOR}{index}");
    if base.len() + suffix.len() <= MAX_TOOL_NAME_LEN {
        return format!("{base}{suffix}");
    }
    let keep = MAX_TOOL_NAME_LEN.saturating_sub(suffix.len());
    format!("{}{}", &base[..keep], suffix)
}

fn sanitize_tool_name(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for ch in value.chars() {
        let normalized = if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-') {
            ch
        } else {
            '_'
        };
        output.push(normalized);
    }
    let output = output.trim_matches('_').to_string();
    if output.is_empty() {
        MCP_TOOL_PREFIX.to_string()
    } else {
        output
    }
}

fn truncate_tool_name(value: &str) -> String {
    if value.len() <= MAX_TOOL_NAME_LEN {
        return value.to_string();
    }
    let suffix = format!("{MCP_TOOL_SEPARATOR}{:08x}", stable_hash(value));
    let keep = MAX_TOOL_NAME_LEN.saturating_sub(suffix.len());
    format!("{}{}", &value[..keep], suffix)
}

fn stable_hash(value: &str) -> u32 {
    let mut hasher = StableHasher::default();
    value.hash(&mut hasher);
    hasher.finish() as u32
}

#[derive(Default)]
struct StableHasher(u64);

impl Hasher for StableHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        let mut hash = if self.0 == 0 {
            0xcbf29ce484222325
        } else {
            self.0
        };
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        self.0 = hash;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolExecutionContext;
    use omini_config::McpServerTransportConfig;
    use std::path::PathBuf;

    fn server_tool(server_name: &str, server_tool_name: &str) -> McpServerToolSpec {
        McpServerToolSpec {
            server_name: server_name.to_string(),
            server_tool_name: server_tool_name.to_string(),
            description: "test tool".to_string(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    fn empty_manager() -> Arc<McpManager> {
        Arc::new(McpManager {
            client_set: McpClientSet::new(
                PathBuf::from("."),
                Vec::<(String, ClientMcpServerConfig)>::new(),
            ),
        })
    }

    fn test_config() -> CoreMcpServerConfig {
        CoreMcpServerConfig {
            transport: McpServerTransportConfig::Stdio {
                command: "server".to_string(),
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
    }

    #[test]
    fn registered_tool_names_are_sanitized() {
        assert_eq!(
            base_registered_tool_name("docs-server", "search docs"),
            "mcp__docs-server__search_docs"
        );
    }

    #[test]
    fn registered_tool_names_are_deduplicated_in_sorted_order() {
        let tools = assign_registered_tool_names(vec![
            server_tool("b", "same name"),
            server_tool("a", "same/name"),
            server_tool("a", "same name"),
        ]);

        let names = tools
            .into_iter()
            .map(|tool| tool.registered_tool_name)
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec![
                "mcp__a__same_name",
                "mcp__a__same_name__1",
                "mcp__b__same_name"
            ]
        );
    }

    #[test]
    fn registered_tool_names_are_truncated_with_stable_suffix() {
        let name = base_registered_tool_name(
            "server-with-a-very-long-name-that-will-not-fit",
            "tool-with-a-very-long-name-that-will-not-fit",
        );

        assert!(name.len() <= MAX_TOOL_NAME_LEN);
        assert!(name.starts_with("mcp__server-with-a-very-long-name"));
    }

    #[test]
    fn client_config_mapping_preserves_mcp_settings() {
        let config = CoreMcpServerConfig {
            startup_timeout_sec: Some(1.5),
            tool_timeout_sec: Some(2.5),
            enabled_tools: Some(vec!["read".to_string()]),
            disabled_tools: Some(vec!["write".to_string()]),
            ..test_config()
        };

        let mapped = client_config_from_core(&config);

        assert_eq!(mapped.startup_timeout_sec, Some(1.5));
        assert_eq!(mapped.tool_timeout_sec, Some(2.5));
        assert_eq!(
            mapped.enabled_tools.as_deref(),
            Some(&["read".to_string()][..])
        );
        assert_eq!(
            mapped.disabled_tools.as_deref(),
            Some(&["write".to_string()][..])
        );
    }

    #[test]
    fn register_server_tools_registers_only_tool_specs() {
        let manager = empty_manager();
        let mut registry = ToolRegistry::new();

        register_server_tools(
            &manager,
            &mut registry,
            vec![
                server_tool("docs", "search"),
                server_tool("docs", "read"),
                server_tool("repo", "search"),
            ],
        );

        assert_eq!(
            registry.tool_names(),
            vec!["mcp__docs__read", "mcp__docs__search", "mcp__repo__search"]
        );
    }

    #[test]
    fn runtime_snapshots_list_mcp_services_and_tools() {
        let status = runtime_snapshots_from_service_snapshots(vec![
            McpServiceSnapshot {
                name: "docs".to_string(),
                status: McpServiceStatus::Ready,
                last_error: None,
                catalog: McpCatalog {
                    tools: vec![server_tool("docs", "search")],
                    ..McpCatalog::default()
                },
            },
            McpServiceSnapshot {
                name: "broken".to_string(),
                status: McpServiceStatus::Failed,
                last_error: Some("boom".to_string()),
                catalog: McpCatalog::default(),
            },
        ]);

        assert_eq!(status.len(), 2);
        assert_eq!(status[0].name, "broken");
        assert_eq!(status[0].status, RuntimeMcpServerStatus::Failed);
        assert_eq!(status[0].last_error.as_deref(), Some("boom"));
        assert!(status[0].tools.is_empty());
        assert_eq!(status[1].name, "docs");
        assert_eq!(status[1].status, RuntimeMcpServerStatus::Ready);
        assert_eq!(status[1].tools.len(), 1);
        assert_eq!(status[1].tools[0].name, "search");
        assert_eq!(status[1].tools[0].registered_name, "mcp__docs__search");
    }

    #[tokio::test]
    async fn runtime_tool_builds_permission_preview_and_error_metadata() {
        let manager = empty_manager();
        let spec = assign_registered_tool_names(vec![server_tool("docs", "search")])
            .into_iter()
            .next()
            .unwrap();
        let tool = McpRuntimeTool {
            manager,
            spec: spec.clone(),
        };
        let prepared = tool
            .prepare(McpToolInput {
                arguments: HashMap::from([("query".to_string(), serde_json::json!("rust"))]),
            })
            .await
            .unwrap();
        let preview = tool
            .permission_preview(&prepared)
            .expect("MCP tool should provide a permission preview");
        let PermissionPreview::Mcp(preview) = preview else {
            panic!("expected MCP permission preview");
        };
        assert_eq!(preview.server_name, "docs");
        assert_eq!(preview.server_tool_name, "search");
        assert_eq!(preview.registered_tool_name, spec.registered_tool_name);
        assert_eq!(preview.inputs["query"], serde_json::json!("rust"));

        let result = tool
            .execute_prepared(
                prepared,
                ToolExecutionContext::test(&spec.registered_tool_name),
            )
            .await;

        assert!(result.is_error);
        assert_eq!(result.output, "Unknown MCP server: docs");
        let metadata = result.metadata.expect("MCP result metadata");
        assert_eq!(metadata["kind"], serde_json::json!("mcp_tool"));
        assert_eq!(metadata["server_name"], serde_json::json!("docs"));
        assert_eq!(metadata["server_tool_name"], serde_json::json!("search"));
        assert_eq!(
            metadata["registered_tool_name"],
            serde_json::json!("mcp__docs__search")
        );
    }
}
