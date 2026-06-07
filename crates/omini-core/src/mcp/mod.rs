use crate::tools::{Tool, ToolRegistry, ToolResult, sanitize_tool_schema, tool_metadata};
use crate::types::config::{McpServerConfig, McpServerTransportConfig, Settings};
use crate::types::events::{McpPermissionPreview, PermissionPreview};
use async_trait::async_trait;
use omini_protocol as protocol;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ClientInfo, GetPromptRequestParams, GetPromptResult,
    Prompt, ReadResourceRequestParams, ReadResourceResult, Resource, ResourceTemplate,
    ServerCapabilities, Tool as RmcpTool,
};
use rmcp::serve_client;
use rmcp::service::{RoleClient, RunningService};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::TokioChildProcess;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::process::Command;
use tokio::sync::OnceCell;
use tracing::Instrument;

const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_TOOL_TIMEOUT: Duration = Duration::from_secs(120);
const MCP_TOOL_PREFIX: &str = "mcp";
const MCP_TOOL_SEPARATOR: &str = "__";
const MAX_TOOL_NAME_LEN: usize = 64;

pub(crate) struct McpManager {
    cwd: PathBuf,
    services: RwLock<HashMap<String, McpService>>,
    startup: OnceCell<Vec<String>>,
}

struct McpService {
    name: String,
    config: McpServerConfig,
    client: Option<Arc<dyn McpServerClient>>,
    status: McpServiceStatus,
    capabilities: Option<ServerCapabilities>,
    catalog: McpCatalog,
    last_error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum McpServiceStatus {
    Disabled,
    Connecting,
    Ready,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct McpServiceSummary {
    pub(crate) name: String,
    pub(crate) status: McpServiceStatus,
    pub(crate) last_error: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct McpCatalog {
    pub(crate) tools: Vec<McpServerToolSpec>,
    pub(crate) resources: Vec<Resource>,
    pub(crate) resource_templates: Vec<ResourceTemplate>,
    pub(crate) prompts: Vec<Prompt>,
}

struct McpServiceReady {
    client: Arc<dyn McpServerClient>,
    capabilities: Option<ServerCapabilities>,
    catalog: McpCatalog,
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

#[derive(Debug, Clone)]
pub(crate) struct McpServerToolSpec {
    server_name: String,
    server_tool_name: String,
    description: String,
    input_schema: Value,
}

#[derive(Debug, Clone)]
struct McpCallOutput {
    content: String,
    is_error: bool,
}

#[async_trait]
trait McpServerClient: Send + Sync {
    fn capabilities(&self) -> Option<ServerCapabilities>;

    async fn list_tools(&self) -> Result<Vec<RmcpTool>, String>;

    async fn call_tool(
        &self,
        server_tool_name: &str,
        arguments: Map<String, Value>,
    ) -> Result<McpCallOutput, String>;

    async fn list_resources(&self) -> Result<Vec<Resource>, String>;

    async fn list_resource_templates(&self) -> Result<Vec<ResourceTemplate>, String>;

    #[allow(dead_code)]
    async fn read_resource(&self, uri: &str) -> Result<ReadResourceResult, String>;

    async fn list_prompts(&self) -> Result<Vec<Prompt>, String>;

    #[allow(dead_code)]
    async fn get_prompt(
        &self,
        name: &str,
        arguments: Option<Map<String, Value>>,
    ) -> Result<GetPromptResult, String>;
}

struct RealMcpServerClient {
    server_name: String,
    service: RunningService<RoleClient, ClientInfo>,
    capabilities: Option<ServerCapabilities>,
    startup_timeout: Duration,
    tool_timeout: Duration,
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

impl McpService {
    fn new(name: String, config: McpServerConfig) -> Self {
        let status = if config.enabled {
            McpServiceStatus::Connecting
        } else {
            McpServiceStatus::Disabled
        };

        Self {
            name,
            config,
            client: None,
            status,
            capabilities: None,
            catalog: McpCatalog::default(),
            last_error: None,
        }
    }
}

impl McpManager {
    pub(crate) fn from_settings(settings: &Settings) -> Self {
        let services = settings
            .mcp_servers
            .iter()
            .map(|(name, config)| (name.clone(), McpService::new(name.clone(), config.clone())))
            .collect();

        Self {
            cwd: settings.cwd.clone(),
            services: RwLock::new(services),
            startup: OnceCell::new(),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.services
            .read()
            .expect("MCP services lock poisoned")
            .is_empty()
    }

    pub(crate) async fn initialize(&self) -> Vec<String> {
        self.startup
            .get_or_init(|| self.initialize_services())
            .await
            .clone()
    }

    async fn initialize_services(&self) -> Vec<String> {
        let inputs = self.service_inputs();
        if inputs.is_empty() {
            return Vec::new();
        }

        let mut handles = Vec::new();
        for (server_name, config, cwd) in inputs {
            self.mark_connecting(&server_name);
            let task_server_name = server_name.clone();
            handles.push(tokio::spawn(
                async move {
                    tracing::debug!(server_name = %server_name, "initializing mcp server");
                    let result = initialize_service(&server_name, config, cwd).await;
                    (server_name, result)
                }
                .instrument(tracing::debug_span!(
                    "mcp_server_initialization",
                    server_name = %task_server_name
                )),
            ));
        }

        let mut warnings = Vec::new();
        for handle in handles {
            match handle.await {
                Ok((server_name, Ok(ready))) => {
                    self.mark_ready(&server_name, ready);
                }
                Ok((server_name, Err(error))) => {
                    self.mark_failed(&server_name, error.clone());
                    warnings.push(format!(
                        "MCP server `{server_name}` failed to start: {error}"
                    ));
                }
                Err(error) => {
                    warnings.push(format!("MCP server initialization task failed: {error}"));
                }
            }
        }
        warnings.sort();
        warnings
    }

    fn service_inputs(&self) -> Vec<(String, McpServerConfig, PathBuf)> {
        let mut inputs = self
            .services
            .read()
            .expect("MCP services lock poisoned")
            .values()
            .filter(|service| service.config.enabled)
            .map(|service| {
                (
                    service.name.clone(),
                    service.config.clone(),
                    self.cwd.clone(),
                )
            })
            .collect::<Vec<_>>();
        inputs.sort_by(|left, right| left.0.cmp(&right.0));
        inputs
    }

    fn mark_connecting(&self, server_name: &str) {
        let mut services = self.services.write().expect("MCP services lock poisoned");
        if let Some(service) = services.get_mut(server_name) {
            service.status = McpServiceStatus::Connecting;
            service.last_error = None;
        }
    }

    fn mark_ready(&self, server_name: &str, ready: McpServiceReady) {
        let mut services = self.services.write().expect("MCP services lock poisoned");
        if let Some(service) = services.get_mut(server_name) {
            service.client = Some(ready.client);
            service.capabilities = ready.capabilities;
            service.catalog = ready.catalog;
            service.status = McpServiceStatus::Ready;
            service.last_error = None;
        }
    }

    fn mark_failed(&self, server_name: &str, error: String) {
        let mut services = self.services.write().expect("MCP services lock poisoned");
        if let Some(service) = services.get_mut(server_name) {
            service.client = None;
            service.catalog = McpCatalog::default();
            service.status = McpServiceStatus::Failed;
            service.last_error = Some(error);
        }
    }

    pub(crate) fn register_available_tools(self: &Arc<Self>, registry: &mut ToolRegistry) {
        let server_tools = self.ready_server_tools();
        for spec in assign_registered_tool_names(server_tools) {
            registry.register(McpRuntimeTool {
                manager: Arc::clone(self),
                spec,
            });
        }
    }

    fn ready_server_tools(&self) -> Vec<McpServerToolSpec> {
        self.services
            .read()
            .expect("MCP services lock poisoned")
            .values()
            .filter(|service| service.status == McpServiceStatus::Ready)
            .flat_map(|service| service.catalog.tools.clone())
            .collect()
    }

    async fn call_tool(
        &self,
        server_name: &str,
        server_tool_name: &str,
        arguments: Map<String, Value>,
    ) -> Result<McpCallOutput, String> {
        let client = self.ready_client(server_name)?;
        client.call_tool(server_tool_name, arguments).await
    }

    #[allow(dead_code)]
    pub(crate) async fn read_resource(
        &self,
        server_name: &str,
        uri: &str,
    ) -> Result<ReadResourceResult, String> {
        let client = self.ready_client(server_name)?;
        client.read_resource(uri).await
    }

    #[allow(dead_code)]
    pub(crate) async fn get_prompt(
        &self,
        server_name: &str,
        name: &str,
        arguments: Option<Map<String, Value>>,
    ) -> Result<GetPromptResult, String> {
        let client = self.ready_client(server_name)?;
        client.get_prompt(name, arguments).await
    }

    fn ready_client(&self, server_name: &str) -> Result<Arc<dyn McpServerClient>, String> {
        let services = self.services.read().expect("MCP services lock poisoned");
        let service = services
            .get(server_name)
            .ok_or_else(|| format!("Unknown MCP server: {server_name}"))?;
        if service.status != McpServiceStatus::Ready {
            return Err(format!(
                "MCP server `{server_name}` is not ready: {:?}",
                service.status
            ));
        }
        service
            .client
            .clone()
            .ok_or_else(|| format!("MCP server `{server_name}` has no active client"))
    }

    #[allow(dead_code)]
    pub(crate) fn services(&self) -> Vec<McpServiceSummary> {
        let mut services = self
            .services
            .read()
            .expect("MCP services lock poisoned")
            .values()
            .map(|service| McpServiceSummary {
                name: service.name.clone(),
                status: service.status,
                last_error: service.last_error.clone(),
            })
            .collect::<Vec<_>>();
        services.sort_by(|left, right| left.name.cmp(&right.name));
        services
    }

    #[allow(dead_code)]
    pub(crate) fn status(&self) -> Vec<McpServiceSummary> {
        self.services()
    }

    pub(crate) fn protocol_status(&self) -> Vec<protocol::SessionRuntimeMcpServer> {
        let services = self.services.read().expect("MCP services lock poisoned");
        let registered_tools = assign_registered_tool_names(
            services
                .values()
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

        let mut snapshots = services
            .values()
            .map(|service| {
                let mut tools = service
                    .catalog
                    .tools
                    .iter()
                    .map(|tool| protocol::SessionRuntimeMcpTool {
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

                protocol::SessionRuntimeMcpServer {
                    name: service.name.clone(),
                    status: mcp_service_status_to_protocol(service.status),
                    last_error: service.last_error.clone(),
                    tools,
                }
            })
            .collect::<Vec<_>>();
        snapshots.sort_by(|left, right| left.name.cmp(&right.name));
        snapshots
    }

    #[allow(dead_code)]
    pub(crate) fn service_status(&self, server_name: &str) -> Option<McpServiceStatus> {
        self.services
            .read()
            .expect("MCP services lock poisoned")
            .get(server_name)
            .map(|service| service.status)
    }

    #[allow(dead_code)]
    pub(crate) fn catalog(&self, server_name: &str) -> Option<McpCatalog> {
        self.services
            .read()
            .expect("MCP services lock poisoned")
            .get(server_name)
            .map(|service| service.catalog.clone())
    }
}

#[async_trait]
impl McpServerClient for RealMcpServerClient {
    fn capabilities(&self) -> Option<ServerCapabilities> {
        self.capabilities.clone()
    }

    async fn list_tools(&self) -> Result<Vec<RmcpTool>, String> {
        tokio::time::timeout(self.startup_timeout, self.service.peer().list_all_tools())
            .await
            .map_err(|_| {
                format!(
                    "MCP server `{}` timed out listing tools after {:?}",
                    self.server_name, self.startup_timeout
                )
            })?
            .map_err(|error| {
                format!(
                    "MCP server `{}` failed to list tools: {error}",
                    self.server_name
                )
            })
    }

    async fn call_tool(
        &self,
        server_tool_name: &str,
        arguments: Map<String, Value>,
    ) -> Result<McpCallOutput, String> {
        let params = CallToolRequestParams {
            meta: None,
            name: server_tool_name.to_string().into(),
            arguments: Some(arguments),
            task: None,
        };
        let result = tokio::time::timeout(self.tool_timeout, self.service.peer().call_tool(params))
            .await
            .map_err(|_| {
                format!(
                    "MCP tool `{server_tool_name}` timed out after {:?}",
                    self.tool_timeout
                )
            })?
            .map_err(|error| format!("MCP tool `{server_tool_name}` failed: {error}"))?;
        Ok(call_output_from_result(result))
    }

    async fn list_resources(&self) -> Result<Vec<Resource>, String> {
        tokio::time::timeout(
            self.startup_timeout,
            self.service.peer().list_all_resources(),
        )
        .await
        .map_err(|_| {
            format!(
                "MCP server `{}` timed out listing resources after {:?}",
                self.server_name, self.startup_timeout
            )
        })?
        .map_err(|error| {
            format!(
                "MCP server `{}` failed to list resources: {error}",
                self.server_name
            )
        })
    }

    async fn list_resource_templates(&self) -> Result<Vec<ResourceTemplate>, String> {
        tokio::time::timeout(
            self.startup_timeout,
            self.service.peer().list_all_resource_templates(),
        )
        .await
        .map_err(|_| {
            format!(
                "MCP server `{}` timed out listing resource templates after {:?}",
                self.server_name, self.startup_timeout
            )
        })?
        .map_err(|error| {
            format!(
                "MCP server `{}` failed to list resource templates: {error}",
                self.server_name
            )
        })
    }

    async fn read_resource(&self, uri: &str) -> Result<ReadResourceResult, String> {
        let params = ReadResourceRequestParams {
            meta: None,
            uri: uri.to_string(),
        };
        tokio::time::timeout(self.tool_timeout, self.service.peer().read_resource(params))
            .await
            .map_err(|_| {
                format!(
                    "MCP resource `{uri}` timed out after {:?}",
                    self.tool_timeout
                )
            })?
            .map_err(|error| format!("MCP resource `{uri}` failed: {error}"))
    }

    async fn list_prompts(&self) -> Result<Vec<Prompt>, String> {
        tokio::time::timeout(self.startup_timeout, self.service.peer().list_all_prompts())
            .await
            .map_err(|_| {
                format!(
                    "MCP server `{}` timed out listing prompts after {:?}",
                    self.server_name, self.startup_timeout
                )
            })?
            .map_err(|error| {
                format!(
                    "MCP server `{}` failed to list prompts: {error}",
                    self.server_name
                )
            })
    }

    async fn get_prompt(
        &self,
        name: &str,
        arguments: Option<Map<String, Value>>,
    ) -> Result<GetPromptResult, String> {
        let params = GetPromptRequestParams {
            meta: None,
            name: name.to_string(),
            arguments,
        };
        tokio::time::timeout(self.tool_timeout, self.service.peer().get_prompt(params))
            .await
            .map_err(|_| {
                format!(
                    "MCP prompt `{name}` timed out after {:?}",
                    self.tool_timeout
                )
            })?
            .map_err(|error| format!("MCP prompt `{name}` failed: {error}"))
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

async fn initialize_service(
    server_name: &str,
    config: McpServerConfig,
    fallback_cwd: PathBuf,
) -> Result<McpServiceReady, String> {
    let client = connect_server(server_name, &config, &fallback_cwd).await?;
    let capabilities = client.capabilities();
    let catalog =
        load_catalog(server_name, &config, client.as_ref(), capabilities.as_ref()).await?;
    Ok(McpServiceReady {
        client,
        capabilities,
        catalog,
    })
}

async fn load_catalog(
    server_name: &str,
    config: &McpServerConfig,
    client: &dyn McpServerClient,
    capabilities: Option<&ServerCapabilities>,
) -> Result<McpCatalog, String> {
    let mut catalog = McpCatalog::default();

    if capabilities.is_some_and(|capabilities| capabilities.tools.is_some()) {
        catalog.tools = client
            .list_tools()
            .await?
            .into_iter()
            .filter(|tool| tool_allowed(config, &tool.name))
            .map(|tool| server_tool_from_rmcp(server_name, tool))
            .collect();
    }

    if capabilities.is_some_and(|capabilities| capabilities.resources.is_some()) {
        catalog.resources = client.list_resources().await?;
        catalog.resource_templates = client.list_resource_templates().await?;
    }

    if capabilities.is_some_and(|capabilities| capabilities.prompts.is_some()) {
        catalog.prompts = client.list_prompts().await?;
    }

    Ok(catalog)
}

fn server_tool_from_rmcp(server_name: &str, tool: RmcpTool) -> McpServerToolSpec {
    let server_tool_name = tool.name.to_string();
    let description = tool
        .description
        .as_deref()
        .map(str::trim)
        .filter(|description| !description.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("MCP tool `{server_name}/{server_tool_name}`."));
    let mut input_schema = Value::Object(tool.input_schema.as_ref().clone());
    sanitize_tool_schema(&mut input_schema);
    McpServerToolSpec {
        server_name: server_name.to_string(),
        server_tool_name,
        description,
        input_schema,
    }
}

async fn connect_server(
    server_name: &str,
    config: &McpServerConfig,
    fallback_cwd: &Path,
) -> Result<Arc<dyn McpServerClient>, String> {
    let startup_timeout = duration_from_secs(config.startup_timeout_sec, DEFAULT_STARTUP_TIMEOUT)?;
    let tool_timeout = duration_from_secs(config.tool_timeout_sec, DEFAULT_TOOL_TIMEOUT)?;
    let service = match &config.transport {
        McpServerTransportConfig::Stdio {
            command,
            args,
            env,
            cwd,
        } => {
            connect_stdio(
                command,
                args,
                env.as_ref(),
                cwd.as_ref(),
                fallback_cwd,
                startup_timeout,
            )
            .await?
        }
        McpServerTransportConfig::StreamableHttp {
            url,
            bearer_token_env_var,
            http_headers,
        } => {
            connect_streamable_http(
                server_name,
                url,
                bearer_token_env_var.as_deref(),
                http_headers.as_ref(),
                startup_timeout,
            )
            .await?
        }
    };
    let capabilities = service
        .peer()
        .peer_info()
        .map(|info| info.capabilities.clone());
    Ok(Arc::new(RealMcpServerClient {
        server_name: server_name.to_string(),
        service,
        capabilities,
        startup_timeout,
        tool_timeout,
    }))
}

async fn connect_stdio(
    command: &str,
    args: &[String],
    env: Option<&HashMap<String, String>>,
    cwd: Option<&PathBuf>,
    fallback_cwd: &Path,
    startup_timeout: Duration,
) -> Result<RunningService<RoleClient, ClientInfo>, String> {
    let mut child = Command::new(command);
    let current_dir = cwd
        .map(PathBuf::from)
        .unwrap_or_else(|| fallback_cwd.to_path_buf());
    child
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .current_dir(current_dir);
    if let Some(env) = env {
        child.envs(env);
    }

    let transport = TokioChildProcess::new(child)
        .map_err(|error| format!("failed to spawn stdio MCP command `{command}`: {error}"))?;
    tokio::time::timeout(startup_timeout, serve_client(client_info(), transport))
        .await
        .map_err(|_| {
            format!("stdio MCP command `{command}` timed out during initialize after {startup_timeout:?}")
        })?
        .map_err(|error| format!("stdio MCP command `{command}` failed to initialize: {error}"))
}

async fn connect_streamable_http(
    server_name: &str,
    url: &str,
    bearer_token_env_var: Option<&str>,
    http_headers: Option<&HashMap<String, String>>,
    startup_timeout: Duration,
) -> Result<RunningService<RoleClient, ClientInfo>, String> {
    let mut config = StreamableHttpClientTransportConfig::with_uri(url.to_string());
    if let Some(env_var) = bearer_token_env_var {
        let token = std::env::var(env_var).map_err(|_| {
            format!("environment variable {env_var} for MCP server `{server_name}` is not set")
        })?;
        if token.trim().is_empty() {
            return Err(format!(
                "environment variable {env_var} for MCP server `{server_name}` is empty"
            ));
        }
        config = config.auth_header(token);
    }

    let headers = build_header_map(http_headers)?;
    let http_client = rmcp_reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .map_err(|error| format!("failed to build MCP HTTP client for `{server_name}`: {error}"))?;
    let transport = StreamableHttpClientTransport::with_client(http_client, config);
    tokio::time::timeout(startup_timeout, serve_client(client_info(), transport))
        .await
        .map_err(|_| {
            format!("MCP HTTP server `{server_name}` timed out during initialize after {startup_timeout:?}")
        })?
        .map_err(|error| format!("MCP HTTP server `{server_name}` failed to initialize: {error}"))
}

fn client_info() -> ClientInfo {
    let mut info = ClientInfo::default();
    info.client_info.name = "omini".to_string();
    info.client_info.title = Some("omini".to_string());
    info
}

fn build_header_map(
    headers: Option<&HashMap<String, String>>,
) -> Result<rmcp_reqwest::header::HeaderMap, String> {
    let mut map = rmcp_reqwest::header::HeaderMap::new();
    let Some(headers) = headers else {
        return Ok(map);
    };
    for (name, value) in headers {
        let name = rmcp_reqwest::header::HeaderName::from_bytes(name.as_bytes())
            .map_err(|error| format!("invalid MCP HTTP header name `{name}`: {error}"))?;
        let value = rmcp_reqwest::header::HeaderValue::from_str(value)
            .map_err(|error| format!("invalid MCP HTTP header value for `{name}`: {error}"))?;
        map.insert(name, value);
    }
    Ok(map)
}

fn duration_from_secs(value: Option<f64>, default: Duration) -> Result<Duration, String> {
    let Some(value) = value else {
        return Ok(default);
    };
    if !value.is_finite() || value <= 0.0 {
        return Err(format!(
            "MCP timeout must be a positive finite number, got {value}"
        ));
    }
    Duration::try_from_secs_f64(value).map_err(|error| error.to_string())
}

fn tool_allowed(config: &McpServerConfig, tool_name: &str) -> bool {
    if let Some(enabled_tools) = &config.enabled_tools
        && !enabled_tools.iter().any(|enabled| enabled == tool_name)
    {
        return false;
    }
    !config
        .disabled_tools
        .as_ref()
        .is_some_and(|disabled_tools| disabled_tools.iter().any(|disabled| disabled == tool_name))
}

fn call_output_from_result(result: CallToolResult) -> McpCallOutput {
    let is_error = result.is_error.unwrap_or(false);
    let content = serde_json::to_string(&result).unwrap_or_else(|error| {
        format!(r#"{{"error":"failed to serialize MCP result: {error}"}}"#)
    });
    McpCallOutput { content, is_error }
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

fn mcp_service_status_to_protocol(status: McpServiceStatus) -> protocol::SessionRuntimeMcpStatus {
    match status {
        McpServiceStatus::Disabled => protocol::SessionRuntimeMcpStatus::Disabled,
        McpServiceStatus::Connecting => protocol::SessionRuntimeMcpStatus::Connecting,
        McpServiceStatus::Ready => protocol::SessionRuntimeMcpStatus::Ready,
        McpServiceStatus::Failed => protocol::SessionRuntimeMcpStatus::Failed,
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
    use crate::types::config::McpServerTransportConfig;
    use rmcp::model::{Annotated, RawResource, RawResourceTemplate};

    fn server_tool(server_name: &str, server_tool_name: &str) -> McpServerToolSpec {
        McpServerToolSpec {
            server_name: server_name.to_string(),
            server_tool_name: server_tool_name.to_string(),
            description: "test tool".to_string(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    fn remote_tool(server_tool_name: &str) -> RmcpTool {
        RmcpTool::new(
            server_tool_name.to_string(),
            "test tool".to_string(),
            Map::from_iter([("type".to_string(), serde_json::json!("object"))]),
        )
    }

    fn resource(uri: &str, name: &str) -> Resource {
        Annotated::new(
            RawResource {
                uri: uri.to_string(),
                name: name.to_string(),
                title: None,
                description: None,
                mime_type: Some("text/plain".to_string()),
                size: None,
                icons: None,
                meta: None,
            },
            None,
        )
    }

    fn resource_template(uri_template: &str, name: &str) -> ResourceTemplate {
        Annotated::new(
            RawResourceTemplate {
                uri_template: uri_template.to_string(),
                name: name.to_string(),
                title: None,
                description: None,
                mime_type: Some("text/plain".to_string()),
                icons: None,
            },
            None,
        )
    }

    fn test_config() -> McpServerConfig {
        McpServerConfig {
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

    fn manager_with_services(services: Vec<McpService>) -> Arc<McpManager> {
        Arc::new(McpManager {
            cwd: PathBuf::from("."),
            services: RwLock::new(
                services
                    .into_iter()
                    .map(|service| (service.name.clone(), service))
                    .collect(),
            ),
            startup: OnceCell::new(),
        })
    }

    fn ready_service(
        name: &str,
        client: Arc<dyn McpServerClient>,
        catalog: McpCatalog,
    ) -> McpService {
        McpService {
            name: name.to_string(),
            config: test_config(),
            client: Some(client),
            status: McpServiceStatus::Ready,
            capabilities: Some(
                ServerCapabilities::builder()
                    .enable_tools()
                    .enable_resources()
                    .enable_prompts()
                    .build(),
            ),
            catalog,
            last_error: None,
        }
    }

    fn failed_service(name: &str, error: &str) -> McpService {
        McpService {
            name: name.to_string(),
            config: test_config(),
            client: None,
            status: McpServiceStatus::Failed,
            capabilities: None,
            catalog: McpCatalog::default(),
            last_error: Some(error.to_string()),
        }
    }

    #[derive(Clone)]
    struct FakeClient {
        output: McpCallOutput,
        tools: Vec<RmcpTool>,
        resources: Vec<Resource>,
        resource_templates: Vec<ResourceTemplate>,
        prompts: Vec<Prompt>,
        capabilities: Option<ServerCapabilities>,
    }

    impl Default for FakeClient {
        fn default() -> Self {
            Self {
                output: McpCallOutput {
                    content: r#"{"content":[{"type":"text","text":"ok"}]}"#.to_string(),
                    is_error: false,
                },
                tools: Vec::new(),
                resources: Vec::new(),
                resource_templates: Vec::new(),
                prompts: Vec::new(),
                capabilities: None,
            }
        }
    }

    #[async_trait]
    impl McpServerClient for FakeClient {
        fn capabilities(&self) -> Option<ServerCapabilities> {
            self.capabilities.clone()
        }

        async fn list_tools(&self) -> Result<Vec<RmcpTool>, String> {
            Ok(self.tools.clone())
        }

        async fn call_tool(
            &self,
            tool_name: &str,
            arguments: Map<String, Value>,
        ) -> Result<McpCallOutput, String> {
            assert_eq!(tool_name, "search");
            assert_eq!(arguments.get("query"), Some(&serde_json::json!("rust")));
            Ok(self.output.clone())
        }

        async fn list_resources(&self) -> Result<Vec<Resource>, String> {
            Ok(self.resources.clone())
        }

        async fn list_resource_templates(&self) -> Result<Vec<ResourceTemplate>, String> {
            Ok(self.resource_templates.clone())
        }

        async fn read_resource(&self, uri: &str) -> Result<ReadResourceResult, String> {
            Ok(ReadResourceResult {
                contents: vec![rmcp::model::ResourceContents::text("content", uri)],
            })
        }

        async fn list_prompts(&self) -> Result<Vec<Prompt>, String> {
            Ok(self.prompts.clone())
        }

        async fn get_prompt(
            &self,
            _name: &str,
            _arguments: Option<Map<String, Value>>,
        ) -> Result<GetPromptResult, String> {
            Ok(GetPromptResult {
                description: None,
                messages: Vec::new(),
            })
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
    fn tool_filter_applies_enabled_before_disabled() {
        let config = McpServerConfig {
            transport: McpServerTransportConfig::Stdio {
                command: "server".to_string(),
                args: Vec::new(),
                env: None,
                cwd: None,
            },
            enabled: true,
            startup_timeout_sec: None,
            tool_timeout_sec: None,
            enabled_tools: Some(vec!["read".to_string(), "write".to_string()]),
            disabled_tools: Some(vec!["write".to_string()]),
        };

        assert!(tool_allowed(&config, "read"));
        assert!(!tool_allowed(&config, "write"));
        assert!(!tool_allowed(&config, "search"));
    }

    #[tokio::test]
    async fn catalog_loads_tools_resources_resource_templates_and_prompts() {
        let client = FakeClient {
            tools: vec![remote_tool("search"), remote_tool("read")],
            resources: vec![resource("file:///docs.md", "docs")],
            resource_templates: vec![resource_template("file:///{path}", "file")],
            prompts: vec![Prompt::new("explain", Some("Explain"), None)],
            capabilities: Some(
                ServerCapabilities::builder()
                    .enable_tools()
                    .enable_resources()
                    .enable_prompts()
                    .build(),
            ),
            ..FakeClient::default()
        };
        let capabilities = client.capabilities();

        let catalog = load_catalog("docs", &test_config(), &client, capabilities.as_ref())
            .await
            .unwrap();

        assert_eq!(catalog.tools.len(), 2);
        assert_eq!(catalog.resources.len(), 1);
        assert_eq!(catalog.resource_templates.len(), 1);
        assert_eq!(catalog.prompts.len(), 1);
    }

    #[test]
    fn manager_registers_all_ready_service_tools() {
        let manager = manager_with_services(vec![
            ready_service(
                "docs",
                Arc::new(FakeClient::default()),
                McpCatalog {
                    tools: vec![server_tool("docs", "search"), server_tool("docs", "read")],
                    ..McpCatalog::default()
                },
            ),
            ready_service(
                "repo",
                Arc::new(FakeClient::default()),
                McpCatalog {
                    tools: vec![server_tool("repo", "search")],
                    ..McpCatalog::default()
                },
            ),
        ]);
        let mut registry = ToolRegistry::new();

        manager.register_available_tools(&mut registry);

        assert_eq!(
            registry.tool_names(),
            vec!["mcp__docs__read", "mcp__docs__search", "mcp__repo__search"]
        );
    }

    #[test]
    fn protocol_status_lists_mcp_services_and_tools() {
        let manager = manager_with_services(vec![
            ready_service(
                "docs",
                Arc::new(FakeClient::default()),
                McpCatalog {
                    tools: vec![server_tool("docs", "search")],
                    ..McpCatalog::default()
                },
            ),
            failed_service("broken", "boom"),
        ]);

        let status = manager.protocol_status();

        assert_eq!(status.len(), 2);
        assert_eq!(status[0].name, "broken");
        assert_eq!(status[0].status, protocol::SessionRuntimeMcpStatus::Failed);
        assert_eq!(status[0].last_error.as_deref(), Some("boom"));
        assert!(status[0].tools.is_empty());
        assert_eq!(status[1].name, "docs");
        assert_eq!(status[1].status, protocol::SessionRuntimeMcpStatus::Ready);
        assert_eq!(status[1].tools.len(), 1);
        assert_eq!(status[1].tools[0].name, "search");
        assert_eq!(status[1].tools[0].registered_name, "mcp__docs__search");
    }

    #[test]
    fn resources_templates_and_prompts_do_not_register_as_tools() {
        let manager = manager_with_services(vec![ready_service(
            "docs",
            Arc::new(FakeClient::default()),
            McpCatalog {
                tools: vec![server_tool("docs", "search")],
                resources: vec![resource("file:///docs.md", "docs")],
                resource_templates: vec![resource_template("file:///{path}", "file")],
                prompts: vec![Prompt::new("explain", Some("Explain"), None)],
            },
        )]);
        let mut registry = ToolRegistry::new();

        manager.register_available_tools(&mut registry);

        assert_eq!(registry.tool_names(), vec!["mcp__docs__search"]);
        let catalog = manager.catalog("docs").unwrap();
        assert_eq!(catalog.resources.len(), 1);
        assert_eq!(catalog.resource_templates.len(), 1);
        assert_eq!(catalog.prompts.len(), 1);
    }

    #[test]
    fn failed_service_does_not_block_ready_service_registration() {
        let manager = manager_with_services(vec![
            ready_service(
                "docs",
                Arc::new(FakeClient::default()),
                McpCatalog {
                    tools: vec![server_tool("docs", "search")],
                    ..McpCatalog::default()
                },
            ),
            failed_service("broken", "boom"),
        ]);
        let mut registry = ToolRegistry::new();

        manager.register_available_tools(&mut registry);

        assert_eq!(registry.tool_names(), vec!["mcp__docs__search"]);
        assert_eq!(
            manager.service_status("broken"),
            Some(McpServiceStatus::Failed)
        );
    }

    #[tokio::test]
    async fn runtime_tool_routes_calls_through_manager() {
        let manager = manager_with_services(vec![ready_service(
            "docs",
            Arc::new(FakeClient {
                output: McpCallOutput {
                    content: r#"{"content":[{"type":"text","text":"ok"}]}"#.to_string(),
                    is_error: false,
                },
                ..FakeClient::default()
            }),
            McpCatalog {
                tools: vec![server_tool("docs", "search")],
                ..McpCatalog::default()
            },
        )]);
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

        assert!(!result.is_error);
        assert_eq!(
            result.output,
            r#"{"content":[{"type":"text","text":"ok"}]}"#
        );
        let metadata = result.metadata.expect("MCP result metadata");
        assert_eq!(metadata["kind"], serde_json::json!("mcp_tool"));
        assert_eq!(metadata["server_name"], serde_json::json!("docs"));
        assert_eq!(metadata["server_tool_name"], serde_json::json!("search"));
        assert_eq!(
            metadata["registered_tool_name"],
            serde_json::json!("mcp__docs__search")
        );
    }

    #[test]
    fn call_output_marks_mcp_error_results_as_tool_errors() {
        let result = CallToolResult {
            content: Vec::new(),
            structured_content: Some(serde_json::json!({"message": "failed"})),
            is_error: Some(true),
            meta: None,
        };

        let output = call_output_from_result(result);

        assert!(output.is_error);
        assert!(output.content.contains("structuredContent"));
    }
}
