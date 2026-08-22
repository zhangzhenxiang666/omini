use crate::skills::SkillRegistry;
use crate::subagents::{AgentRegistry, AgentTaskSupervisor};
use crate::types::events::EngineToRuntimeEvent;
use async_trait::async_trait;
use omini_config::Settings;
use omini_config::project::{ProjectDir, ThreadDir};
use omini_domain::events::{
    ActiveProfile, PermissionPreview, PermissionSource, ToolPauseKind, ToolPauseRequest,
    ToolPauseResponse, UserInputPreview,
};
use omini_domain::message::{ContentBlock, ToolResultBlock};
use omini_domain::tool::ToolDefinition;
use omini_permissions::{PermissionDecision, PermissionEngine};
use schemars::JsonSchema;
use schemars::generate::SchemaSettings;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{Notify, mpsc, oneshot};

pub mod agent_tools;
pub mod ask_user_tool;
pub mod bash_tool;
pub mod edit_tool;
pub mod read_tool;
pub mod search_tool;
pub mod skill_tool;
pub mod todo_tool;
pub mod view_image_tool;
pub mod write_tool;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
type ToolExecutor = dyn Fn(HashMap<String, Value>, ToolExecutionContext) -> BoxFuture<'static, ToolResult>
    + Send
    + Sync;

/// 去除 serde_json 错误信息末尾的 " at line X column Y" 位置信息，
fn clean_json_error(e: &serde_json::Error) -> String {
    let msg = e.to_string();
    match msg.split_once(" at line ") {
        Some((body, _)) => body.to_string(),
        None => msg,
    }
}

fn permission_denied_result(tool_name: &str, note: Option<&str>) -> ToolResult {
    let message = format!("Permission denied for tool: {tool_name}");
    let user_note_present = note.map(str::trim).is_some_and(|note| !note.is_empty());
    let metadata = tool_metadata([
        ("permission_denied", serde_json::json!(true)),
        ("user_note_present", serde_json::json!(user_note_present)),
        ("permission_denial_source", serde_json::json!("user")),
    ]);
    let Some(note) = note.map(str::trim).filter(|note| !note.is_empty()) else {
        return ToolResult::error(message).with_metadata(metadata);
    };

    let value = serde_json::json!({
        "error": "permission_denied",
        "message": message,
        "user_guidance": note,
        "required_action": "retry_with_user_guidance",
    });
    ToolResult::error(serde_json::to_string(&value).unwrap_or_else(|_| value.to_string()))
        .with_metadata(metadata)
}

/// 工具执行后的内部结果
#[derive(Debug, Clone)]
pub struct ToolResult {
    pub output: String,
    pub is_error: bool,
    pub metadata: Option<Map<String, Value>>,
    pub extra_blocks: Option<Vec<ContentBlock>>,
}

pub fn tool_metadata<const N: usize>(entries: [(&str, Value); N]) -> Map<String, Value> {
    entries
        .into_iter()
        .map(|(key, value)| (key.to_string(), value))
        .collect()
}

impl ToolResult {
    pub fn ok(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            is_error: false,
            metadata: None,
            extra_blocks: None,
        }
    }

    pub fn error(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            is_error: true,
            metadata: None,
            extra_blocks: None,
        }
    }

    pub fn with_metadata(mut self, metadata: Map<String, Value>) -> Self {
        self.metadata = Some(metadata);
        self
    }

    pub fn with_extra_blocks(mut self, blocks: Vec<ContentBlock>) -> Self {
        self.extra_blocks = Some(blocks);
        self
    }

    pub fn into_parts(self, tool_use_id: &str) -> (ToolResultBlock, Option<Vec<ContentBlock>>) {
        let block = ToolResultBlock {
            tool_use_id: tool_use_id.to_string(),
            is_error: self.is_error,
            content: self.output,
            metadata: self.metadata,
        };
        (block, self.extra_blocks)
    }
}

pub type PendingToolPauses = Arc<Mutex<HashMap<String, PendingToolPause>>>;

#[derive(Debug)]
pub enum PendingToolPause {
    Permission(oneshot::Sender<ToolPauseResponse>),
    UserInput(oneshot::Sender<ToolPauseResponse>),
}

#[derive(Clone)]
pub struct ToolExecutionContext {
    pub tool_use_id: String,
    pub pause_id: String,
    pub tool_name: String,
    pub settings: Arc<Settings>,
    pub tool_registry: Arc<ToolRegistry>,
    pub event_tx: mpsc::Sender<EngineToRuntimeEvent>,
    pub pending_tool_pauses: PendingToolPauses,
    pub permission_engine: Arc<PermissionEngine>,
    pub active_profile: ActiveProfile,
    pub cancelled: Arc<AtomicBool>,
    #[allow(dead_code)] // Retained in the per-tool context for cancellation-aware tool handlers.
    pub cancel_notify: Arc<Notify>,
    pub runtime: Option<Arc<ToolRuntimeContext>>,
}

#[derive(Clone)]
pub struct ToolRuntimeContext {
    pub thread_id: String,
    pub run_id: Option<String>,
    pub thread_type: String,
    pub agent_label: Option<String>,
    pub thread_dir: ThreadDir,
    pub llm_context_version: Arc<AtomicI64>,
    pub agent_depth: u8,
    pub task_id: Option<String>,
    pub owner_thread_id: String,
    pub agent_registry: Arc<AgentRegistry>,
    pub skill_registry: Arc<SkillRegistry>,
    pub task_supervisor: Option<Arc<AgentTaskSupervisor>>,
    pub project: ProjectDir,
}

impl std::fmt::Debug for ToolRuntimeContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolRuntimeContext")
            .field("thread_id", &self.thread_id)
            .field("run_id", &self.run_id)
            .field("thread_type", &self.thread_type)
            .field("agent_label", &self.agent_label)
            .field("thread_dir", &self.thread_dir)
            .field(
                "llm_context_version",
                &self.llm_context_version.load(Ordering::Relaxed),
            )
            .field("agent_depth", &self.agent_depth)
            .field("task_id", &self.task_id)
            .field("owner_thread_id", &self.owner_thread_id)
            .field("agent_registry", &self.agent_registry)
            .field("skill_registry", &self.skill_registry)
            .field("project", &self.project)
            .finish_non_exhaustive()
    }
}

impl ToolExecutionContext {
    #[cfg(test)]
    pub fn test(tool_name: &str) -> Self {
        Self::test_with_cwd(
            tool_name,
            std::env::current_dir().unwrap_or_else(|_| ".".into()),
        )
    }

    #[cfg(test)]
    pub fn test_with_cwd(tool_name: &str, cwd: PathBuf) -> Self {
        use omini_config::{CompactConfig, ModelTiers, ProviderProfile, Settings};
        use omini_domain::config::{ModelInfo, ProviderEndpointKind};

        let (event_tx, _event_rx) = mpsc::channel(1);
        let mut providers = HashMap::new();
        providers.insert(
            "test".to_string(),
            ProviderProfile {
                name: "Test".to_string(),
                endpoint: ProviderEndpointKind::OpenAI,
                api_key: String::new(),
                base_url: String::new(),
                models: vec![ModelInfo {
                    id: "test-model".to_string(),
                    name: None,
                    limit: 256000,
                    thinking: false,
                    input_modalities: None,
                    extra_body: None,
                    extra_headers: None,
                }],
            },
        );
        Self {
            tool_use_id: format!("test_{tool_name}"),
            pause_id: format!("test_{tool_name}"),
            tool_name: tool_name.to_string(),
            settings: Arc::new(Settings {
                api_key: String::new(),
                base_url: String::new(),
                model: "test-model".to_string(),
                endpoint: ProviderEndpointKind::OpenAI,
                providers,
                active_provider: "test".to_string(),
                system_prompt: None,
                language: None,
                max_turns: None,
                cwd,
                thinking_effort: None,
                permissions: None,
                compact: CompactConfig::default(),
                mcp_servers: HashMap::new(),
                model_tiers: ModelTiers::default(),
            }),
            tool_registry: Arc::new(ToolRegistry::new()),
            event_tx,
            pending_tool_pauses: Arc::new(Mutex::new(HashMap::new())),
            permission_engine: Arc::new(PermissionEngine::empty(
                std::env::current_dir().unwrap_or_else(|_| ".".into()),
            )),
            active_profile: ActiveProfile::Main,
            cancelled: Arc::new(AtomicBool::new(false)),
            cancel_notify: Arc::new(Notify::new()),
            runtime: None,
        }
    }

    pub async fn request_permission(
        &self,
        preview: PermissionPreview,
        permission_source: Option<PermissionSource>,
    ) -> ToolPauseResponse {
        if self.cancelled.load(Ordering::Relaxed) {
            return ToolPauseResponse::Cancelled;
        }

        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self
                .pending_tool_pauses
                .lock()
                .expect("pending tool pause mutex poisoned");
            if pending.contains_key(&self.pause_id) {
                return ToolPauseResponse::Permission {
                    approved: false,
                    note: None,
                };
            }
            pending.insert(self.pause_id.clone(), PendingToolPause::Permission(tx));
        }

        let request = ToolPauseRequest {
            tool_use_id: self.pause_id.clone(),
            preview_tool_use_id: preview_tool_use_id(&self.pause_id, &self.tool_use_id),
            tool_name: self.tool_name.clone(),
            permission_source,
            source_thread_id: self
                .runtime
                .as_ref()
                .map(|runtime| runtime.thread_id.clone()),
            source_agent_label: self
                .runtime
                .as_ref()
                .and_then(|runtime| runtime.agent_label.clone()),
            kind: ToolPauseKind::Permission(preview),
        };

        if self
            .event_tx
            .send(EngineToRuntimeEvent::ToolPauseRequested(Box::new(request)))
            .await
            .is_err()
        {
            self.remove_pending_pause();
            return ToolPauseResponse::Cancelled;
        }

        match rx.await {
            Ok(response) => response,
            Err(_) => ToolPauseResponse::Cancelled,
        }
    }

    pub async fn request_user_input(&self, preview: UserInputPreview) -> ToolPauseResponse {
        if self.cancelled.load(Ordering::Relaxed) {
            return ToolPauseResponse::Cancelled;
        }

        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self
                .pending_tool_pauses
                .lock()
                .expect("pending tool pause mutex poisoned");
            if pending.contains_key(&self.pause_id) {
                return ToolPauseResponse::Cancelled;
            }
            pending.insert(self.pause_id.clone(), PendingToolPause::UserInput(tx));
        }

        let request = ToolPauseRequest {
            tool_use_id: self.pause_id.clone(),
            preview_tool_use_id: preview_tool_use_id(&self.pause_id, &self.tool_use_id),
            tool_name: self.tool_name.clone(),
            permission_source: None,
            source_thread_id: self
                .runtime
                .as_ref()
                .map(|runtime| runtime.thread_id.clone()),
            source_agent_label: self
                .runtime
                .as_ref()
                .and_then(|runtime| runtime.agent_label.clone()),
            kind: ToolPauseKind::UserInput(preview),
        };

        if self
            .event_tx
            .send(EngineToRuntimeEvent::ToolPauseRequested(Box::new(request)))
            .await
            .is_err()
        {
            self.remove_pending_pause();
            return ToolPauseResponse::Cancelled;
        }

        match rx.await {
            Ok(response) => response,
            Err(_) => ToolPauseResponse::Cancelled,
        }
    }

    fn remove_pending_pause(&self) {
        let mut pending = self
            .pending_tool_pauses
            .lock()
            .expect("pending tool pause mutex poisoned");
        pending.remove(&self.pause_id);
    }
}

fn preview_tool_use_id(pause_id: &str, tool_use_id: &str) -> Option<String> {
    (pause_id != tool_use_id).then(|| tool_use_id.to_string())
}

pub fn normalize_tool_paths(tool_name: &str, raw_input: &mut Value, cwd: &Path) {
    match tool_name {
        "bash" => normalize_path_field(raw_input, "workdir", cwd, true),
        "search" => normalize_path_field(raw_input, "path", cwd, true),
        "read" | "edit" | "write" => normalize_path_field(raw_input, "file_path", cwd, false),
        "view_image" => normalize_path_field(raw_input, "path", cwd, false),
        _ => {}
    }
}

fn normalize_path_field(raw_input: &mut Value, field: &str, cwd: &Path, default_to_cwd: bool) {
    let Some(input) = raw_input.as_object_mut() else {
        return;
    };

    match input.get_mut(field) {
        Some(Value::String(path)) => {
            let raw = path.trim();
            if raw.is_empty() {
                if default_to_cwd {
                    *path = cwd.display().to_string();
                }
                return;
            }
            *path = resolve_thread_path(cwd, raw).display().to_string();
        }
        Some(Value::Null) if default_to_cwd => {
            input.insert(field.to_string(), Value::String(cwd.display().to_string()));
        }
        None if default_to_cwd => {
            input.insert(field.to_string(), Value::String(cwd.display().to_string()));
        }
        _ => {}
    }
}

fn resolve_thread_path(cwd: &Path, raw: &str) -> PathBuf {
    let path = Path::new(raw);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

#[async_trait]
pub trait Tool: Send + Sync + 'static {
    /// 每个工具关联自己的输入参数结构体（需派生 JsonSchema + Deserialize）
    type Input: DeserializeOwned + JsonSchema + Send;
    type Prepared: Send + 'static;

    fn name(&self) -> &str;
    fn description(&self) -> &str;

    /// 自动从 Self::Input 生成 JSON Schema
    fn input_schema(&self) -> Value {
        let mut schema = serde_json::to_value(
            SchemaSettings::default()
                .with(|settings| {
                    settings.meta_schema = None;
                    settings.inline_subschemas = true;
                })
                .into_generator()
                .into_root_schema_for::<Self::Input>(),
        )
        .unwrap();
        sanitize_tool_schema(&mut schema);
        schema
    }

    /// 预检查工具调用，生成内部强类型执行计划。
    async fn prepare(&self, input: Self::Input) -> Result<Self::Prepared, ToolResult>;

    /// 生成对外可序列化的权限预览。返回 None 表示无需权限审批。
    fn permission_preview(&self, _prepared: &Self::Prepared) -> Option<PermissionPreview> {
        None
    }

    /// 执行已经预检查过的工具计划。
    async fn execute_prepared(
        &self,
        prepared: Self::Prepared,
        ctx: ToolExecutionContext,
    ) -> ToolResult;
}

pub fn sanitize_tool_schema(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.remove("$schema");
            map.remove("default");
            map.remove("format");
            map.remove("title");

            if let Some(Value::Array(types)) = map.get("type") {
                let mut types = types.clone();
                types.retain(|ty| ty.as_str() != Some("null"));
                if types.len() == 1 {
                    map.insert("type".to_string(), types[0].clone());
                }
            }

            if map.get("type").and_then(Value::as_str) == Some("object") {
                map.entry("additionalProperties".to_string())
                    .or_insert(Value::Bool(false));
            }

            for value in map.values_mut() {
                sanitize_tool_schema(value);
            }
        }
        Value::Array(values) => {
            for value in values {
                sanitize_tool_schema(value);
            }
        }
        _ => {}
    }
}

/// 注册表中存的「已擦除类型」的工具。
pub struct RegisteredTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    executor: Arc<ToolExecutor>,
}

impl Clone for RegisteredTool {
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            description: self.description.clone(),
            input_schema: self.input_schema.clone(),
            executor: Arc::clone(&self.executor),
        }
    }
}

impl RegisteredTool {
    pub fn new<T: Tool>(tool: T) -> Self {
        let name = tool.name().to_string();
        let description = tool.description().to_string();
        let input_schema = tool.input_schema();
        let tool = Arc::new(tool);
        let executor: Arc<ToolExecutor> = Arc::new(
            move |input: HashMap<String, Value>, ctx: ToolExecutionContext| {
                let tool = Arc::clone(&tool);
                Box::pin(async move {
                    let mut raw_input = Value::Object(input.clone().into_iter().collect());
                    normalize_tool_paths(tool.name(), &mut raw_input, &ctx.settings.cwd);
                    if let Some(check) = ctx
                        .permission_engine
                        .profile_policy(ctx.active_profile, tool.name())
                        && let PermissionDecision::Deny { reason } = check.decision
                    {
                        return ToolResult::error(reason);
                    }

                    let input: T::Input = match serde_json::from_value(raw_input.clone()) {
                        Ok(i) => i,
                        Err(e) => {
                            return ToolResult::error(format!(
                                "Invalid tool input: {}",
                                clean_json_error(&e)
                            ));
                        }
                    };
                    let prepared = match tool.prepare(input).await {
                        Ok(prepared) => prepared,
                        Err(result) => return result,
                    };

                    let preview = tool.permission_preview(&prepared);
                    let permission_check = ctx.permission_engine.check_for_profile(
                        ctx.active_profile,
                        tool.name(),
                        preview.as_ref(),
                        &raw_input,
                    );
                    match permission_check.decision {
                        PermissionDecision::Allow => {}
                        PermissionDecision::Deny { reason } => return ToolResult::error(reason),
                        PermissionDecision::Ask => {
                            let preview = preview.unwrap_or_else(|| PermissionPreview::Custom {
                                tool_name: tool.name().to_string(),
                                payload: raw_input.as_object().cloned().unwrap_or_default(),
                            });
                            match ctx
                                .request_permission(preview, permission_check.source)
                                .await
                            {
                                ToolPauseResponse::Permission { approved: true, .. } => {}
                                ToolPauseResponse::Permission {
                                    approved: false,
                                    note,
                                } => {
                                    return permission_denied_result(tool.name(), note.as_deref());
                                }
                                ToolPauseResponse::Cancelled => {
                                    return ToolResult::error("Tool execution cancelled");
                                }
                                ToolPauseResponse::UserInput { .. } => {
                                    return ToolResult::error(
                                        "Received user input response for permission request",
                                    );
                                }
                            }
                        }
                    }

                    tool.execute_prepared(prepared, ctx).await
                })
            },
        );
        Self {
            name,
            description,
            input_schema,
            executor,
        }
    }

    /// 返回工具定义（名称、描述、输入 schema），供 LLM API 注册使用。
    pub fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: self.name.clone(),
            description: self.description.clone(),
            input_schema: self.input_schema.clone(),
        }
    }

    pub async fn execute(
        &self,
        input: HashMap<String, Value>,
        ctx: ToolExecutionContext,
    ) -> ToolResult {
        (self.executor)(input, ctx).await
    }
}

/// 工具注册表。
#[derive(Default)]
pub struct ToolRegistry {
    tools: HashMap<String, RegisteredTool>,
}

impl Clone for ToolRegistry {
    fn clone(&self) -> Self {
        Self {
            tools: self.tools.clone(),
        }
    }
}

impl std::fmt::Debug for ToolRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolRegistry")
            .field("tools", &self.tools.keys())
            .finish()
    }
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register<T: Tool>(&mut self, tool: T) -> &mut Self {
        let reg = RegisteredTool::new(tool);
        let name = reg.name.clone();
        self.tools.insert(name, reg);
        self
    }

    pub fn get(&self, name: &str) -> Option<&RegisteredTool> {
        self.tools.get(name)
    }

    /// 返回所有已注册工具的 `ToolDefinition` 列表（供 LLM API 注册使用）
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        let mut definitions: Vec<_> = self.tools.values().map(|tool| tool.definition()).collect();
        definitions.sort_by(|left, right| {
            tool_definition_priority(&left.name)
                .cmp(&tool_definition_priority(&right.name))
                .then_with(|| left.name.cmp(&right.name))
        });
        definitions
    }

    pub fn contains(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    pub fn tool_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.tools.keys().cloned().collect();
        names.sort();
        names
    }

    pub fn filtered(&self, allowed_tools: &[String]) -> Self {
        let allowed: HashSet<&str> = allowed_tools.iter().map(String::as_str).collect();
        let tools = self
            .tools
            .iter()
            .filter(|(name, _)| allowed.contains(name.as_str()))
            .map(|(name, tool)| (name.clone(), tool.clone()))
            .collect();
        Self { tools }
    }
}

pub fn create_main_registry() -> ToolRegistry {
    create_registry_with_allowed(None, AgentToolSet::Main)
}

pub fn create_agent_registry_from_parent(
    parent: &ToolRegistry,
    allow: Option<&[String]>,
    deny: &[String],
    depth: u8,
) -> Result<(ToolRegistry, Vec<String>), String> {
    let mut warnings = Vec::new();
    let parent_names: HashSet<String> = parent.tool_names().into_iter().collect();
    let deny_names: HashSet<&str> = deny.iter().map(String::as_str).collect();
    let mut selected = Vec::new();

    match allow {
        Some(allow) => {
            for name in allow {
                if is_agent_control_tool(name) {
                    continue;
                }
                if deny_names.contains(name.as_str()) {
                    continue;
                }
                if parent_names.contains(name) {
                    selected.push(name.clone());
                } else {
                    warnings.push(format!(
                        "tool '{name}' is not available to the parent agent"
                    ));
                }
            }
        }
        None => {
            selected.extend(
                parent_names
                    .iter()
                    .filter(|name| !is_agent_control_tool(name))
                    .filter(|name| !deny_names.contains(name.as_str()))
                    .cloned(),
            );
        }
    }

    for name in deny {
        if !is_agent_control_tool(name) && !parent_names.contains(name) {
            warnings.push(format!(
                "disallowed tool '{name}' is not available to the parent agent"
            ));
        }
    }

    selected.sort();
    selected.dedup();
    if selected.is_empty() {
        if !warnings.is_empty() {
            return Err(format!(
                "agent tool policy leaves no available tools: {}",
                warnings.join("; ")
            ));
        }
        return Err("agent tool policy leaves no available tools".to_string());
    }

    let mut registry = parent.filtered(&selected);
    let run_agent_allowed = depth < omini_domain::events::MAX_AGENT_DEPTH
        && allow.is_none_or(|tools| tools.iter().any(|tool| tool == "run_agent"))
        && !deny_names.contains("run_agent");
    if run_agent_allowed {
        registry.register(agent_tools::RunAgentTool);
    }
    Ok((registry, warnings))
}

fn create_registry_with_allowed(
    allowed: Option<&[String]>,
    agent_tool_set: AgentToolSet,
) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    if tool_allowed(allowed, "ask_user") {
        registry.register(ask_user_tool::AskUserTool);
    }
    if tool_allowed(allowed, "skill") {
        registry.register(skill_tool::SkillTool);
    }
    if tool_allowed(allowed, "bash") {
        registry.register(bash_tool::BashTool);
    }
    if tool_allowed(allowed, "search") {
        registry.register(search_tool::SearchTool);
    }
    if tool_allowed(allowed, "read") {
        registry.register(read_tool::ReadTool);
    }
    if tool_allowed(allowed, "view_image") {
        registry.register(view_image_tool::ViewImageTool);
    }
    if tool_allowed(allowed, "edit") {
        registry.register(edit_tool::EditTool);
    }
    if tool_allowed(allowed, "write") {
        registry.register(write_tool::WriteTool);
    }
    if agent_tool_set == AgentToolSet::Main && tool_allowed(allowed, "todo_write") {
        registry.register(todo_tool::TodoWriteTool);
    }
    if agent_tool_set == AgentToolSet::Main {
        registry.register(agent_tools::SpawnAgentTool);
        registry.register(agent_tools::GetTaskTool);
        registry.register(agent_tools::CancelTaskTool);
    }
    registry
}

fn tool_allowed(allowed: Option<&[String]>, name: &str) -> bool {
    allowed.is_none_or(|tools| tools.iter().any(|tool| tool == name))
}

fn tool_definition_priority(name: &str) -> usize {
    match name {
        "search" => 0,
        "read" => 1,
        "view_image" => 2,
        "edit" => 3,
        "write" => 4,
        "bash" => 5,
        "ask_user" => 6,
        "skill" => 7,
        "todo_write" => 8,
        "spawn_agent" => 9,
        "run_agent" => 10,
        "get_task" => 11,
        "cancel_task" => 12,
        _ => 100,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AgentToolSet {
    Main,
}

fn is_agent_control_tool(name: &str) -> bool {
    matches!(
        name,
        "spawn_agent" | "run_agent" | "get_task" | "cancel_task"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_search_default_path_uses_thread_cwd() {
        let cwd = Path::new("/repo");
        let mut input = serde_json::json!({ "query": "needle" });

        normalize_tool_paths("search", &mut input, cwd);

        assert_eq!(input["path"], serde_json::json!("/repo"));
    }

    #[test]
    fn normalize_bash_relative_workdir_uses_thread_cwd() {
        let cwd = Path::new("/repo");
        let mut input = serde_json::json!({
            "command": "cargo check",
            "workdir": "crates/omini-core"
        });

        normalize_tool_paths("bash", &mut input, cwd);

        assert_eq!(
            input["workdir"],
            serde_json::json!("/repo/crates/omini-core")
        );
    }

    #[test]
    fn normalize_file_tool_relative_file_path_uses_thread_cwd() {
        let cwd = Path::new("/repo");
        let mut input = serde_json::json!({ "file_path": "src/lib.rs" });

        normalize_tool_paths("read", &mut input, cwd);

        assert_eq!(input["file_path"], serde_json::json!("/repo/src/lib.rs"));
    }

    #[test]
    fn normalize_absolute_path_keeps_path() {
        let cwd = Path::new("/repo");
        let mut input = serde_json::json!({ "path": "/tmp/image.png" });

        normalize_tool_paths("view_image", &mut input, cwd);

        assert_eq!(input["path"], serde_json::json!("/tmp/image.png"));
    }

    #[test]
    fn permission_denied_without_note_preserves_plain_message() {
        let result = permission_denied_result("bash", None);

        assert!(result.is_error);
        assert_eq!(result.output, "Permission denied for tool: bash");
        let metadata = result.metadata.expect("permission denial metadata");
        assert_eq!(metadata["permission_denied"], serde_json::json!(true));
        assert_eq!(metadata["user_note_present"], serde_json::json!(false));
        assert_eq!(
            metadata["permission_denial_source"],
            serde_json::json!("user")
        );
    }

    #[test]
    fn permission_denied_with_note_returns_guidance_content() {
        let result = permission_denied_result("bash", Some("Please inspect first."));
        let value: Value = serde_json::from_str(&result.output).unwrap();

        assert!(result.is_error);
        assert!(!result.output.contains('\n'));
        assert_eq!(value["error"], "permission_denied");
        assert_eq!(value["message"], "Permission denied for tool: bash");
        assert_eq!(value["user_guidance"], "Please inspect first.");
        assert_eq!(value["required_action"], "retry_with_user_guidance");
        assert!(value.get("tool").is_none());
        assert!(value.get("next_step").is_none());
        assert!(value.get("user_note").is_none());
        let metadata = result.metadata.expect("permission denial metadata");
        assert_eq!(metadata["permission_denied"], serde_json::json!(true));
        assert_eq!(metadata["user_note_present"], serde_json::json!(true));
        assert_eq!(
            metadata["permission_denial_source"],
            serde_json::json!("user")
        );
    }

    #[test]
    fn permission_denied_note_uses_json_string_escaping() {
        let result = permission_denied_result("bash", Some("Use A < B & C > D."));
        let value: Value = serde_json::from_str(&result.output).unwrap();

        assert_eq!(value["user_guidance"], "Use A < B & C > D.");
    }

    #[test]
    fn main_registry_exposes_ordered_tool_contracts() {
        let registry = create_main_registry();
        assert_eq!(
            registry.tool_names(),
            vec![
                "ask_user",
                "bash",
                "cancel_task",
                "edit",
                "get_task",
                "read",
                "search",
                "skill",
                "spawn_agent",
                "todo_write",
                "view_image",
                "write",
            ]
        );
        assert_eq!(
            registry
                .definitions()
                .into_iter()
                .map(|definition| definition.name)
                .collect::<Vec<_>>(),
            vec![
                "search",
                "read",
                "view_image",
                "edit",
                "write",
                "bash",
                "ask_user",
                "skill",
                "todo_write",
                "spawn_agent",
                "get_task",
                "cancel_task",
            ]
        );

        let todo = registry
            .definitions()
            .into_iter()
            .find(|definition| definition.name == "todo_write")
            .expect("todo_write definition");
        let todo_item = &todo.input_schema["properties"]["todos"]["items"]["properties"];
        assert!(todo_item["content"].is_object());
        assert!(todo_item["status"].is_object());
        assert!(todo_item.get("step").is_none());

        for schema in [
            agent_tools::SpawnAgentTool.input_schema(),
            agent_tools::RunAgentTool.input_schema(),
        ] {
            let required = schema["required"].as_array().expect("required fields");
            for field in ["name", "prompt", "title"] {
                assert!(required.iter().any(|value| value.as_str() == Some(field)));
            }
        }
        assert!(
            agent_tools::SpawnAgentTool
                .description()
                .contains("automatic notification")
        );
    }

    #[test]
    fn agent_registry_applies_parent_allow_deny_and_depth_rules() {
        let parent = create_main_registry();
        let allow = vec![
            "read".to_string(),
            "search".to_string(),
            "write".to_string(),
            "run_agent".to_string(),
            "missing".to_string(),
        ];
        let deny = vec!["write".to_string()];
        let (child, warnings) = create_agent_registry_from_parent(&parent, Some(&allow), &deny, 1)
            .expect("remaining allowed tools should create a registry");

        assert_eq!(child.tool_names(), vec!["read", "run_agent", "search"]);
        assert_eq!(
            warnings,
            vec!["tool 'missing' is not available to the parent agent"]
        );

        let (deep_child, deep_warnings) = create_agent_registry_from_parent(&parent, None, &[], 2)
            .expect("default policy should retain ordinary tools");
        assert!(!deep_child.contains("run_agent"));
        assert!(deep_warnings.is_empty());
    }

    #[test]
    fn tool_result_preserves_error_metadata_and_extra_blocks() {
        let result = ToolResult::error("failed")
            .with_metadata(tool_metadata([("kind", serde_json::json!("test"))]))
            .with_extra_blocks(vec![omini_domain::message::ContentBlock::from_text(
                "extra".into(),
            )]);

        let (block, extra_blocks) = result.into_parts("call-1");
        assert_eq!(block.tool_use_id, "call-1");
        assert!(block.is_error);
        assert_eq!(block.content, "failed");
        assert_eq!(
            block.metadata,
            Some(tool_metadata([("kind", serde_json::json!("test"))]))
        );
        assert_eq!(
            extra_blocks,
            Some(vec![omini_domain::message::ContentBlock::from_text(
                "extra".into()
            )])
        );

        assert_eq!(ToolResult::ok("done").extra_blocks, None);
    }

    #[tokio::test]
    async fn todo_tool_rejects_empty_input_and_serializes_full_list() {
        let empty = todo_tool::TodoWriteTool
            .prepare(todo_tool::TodoWriteInput { todos: Vec::new() })
            .await
            .expect_err("empty todo list should reject");
        assert!(empty.is_error);
        assert_eq!(empty.output, "todos must contain at least one item");
        assert_eq!(empty.metadata, None);
        assert_eq!(empty.extra_blocks, None);

        let input = todo_tool::TodoWriteInput {
            todos: vec![todo_tool::TodoItemInput {
                content: "Implement focused tests".into(),
                status: todo_tool::TodoStatus::InProgress,
            }],
        };
        let prepared = todo_tool::TodoWriteTool
            .prepare(input)
            .await
            .expect("valid todo should prepare");
        assert_eq!(prepared.todos.len(), 1);
        assert_eq!(prepared.todos[0].content, "Implement focused tests");
    }

    #[tokio::test]
    async fn file_tools_apply_and_reject_paths() {
        let temp = crate::test_support::TestTempDir::new("file-tools");
        let file = temp.write("nested/note.txt", "first\nsecond\n");
        let path = file.display().to_string();

        let read = read_tool::ReadTool
            .prepare(read_tool::ReadInput {
                file_path: path.clone(),
                offset: None,
                limit: None,
            })
            .await
            .expect("absolute file should prepare");
        let read_result = read_tool::ReadTool
            .execute_prepared(
                read,
                crate::test_support::tool_context(temp.path(), "read", false),
            )
            .await;
        assert!(!read_result.is_error);
        assert_eq!(read_result.output, "1: first\n2: second");
        assert_eq!(read_result.metadata, None);
        assert_eq!(read_result.extra_blocks, None);

        let edit = edit_tool::EditTool
            .prepare(edit_tool::EditInput {
                file_path: path.clone(),
                old_string: "second".into(),
                new_string: "SECOND".into(),
                replace_all: None,
            })
            .await
            .expect("unique edit should prepare");
        let edit_result = edit_tool::EditTool
            .execute_prepared(
                edit,
                crate::test_support::tool_context(temp.path(), "edit", false),
            )
            .await;
        assert!(!edit_result.is_error, "{}", edit_result.output);
        assert_eq!(
            std::fs::read_to_string(&file).expect("edited file should read"),
            "first\nSECOND\n"
        );

        let write_result = write_tool::WriteTool
            .execute_prepared(
                write_tool::WriteTool
                    .prepare(write_tool::WriteInput {
                        file_path: path,
                        content: "replacement\n".into(),
                    })
                    .await
                    .expect("absolute write should prepare"),
                crate::test_support::tool_context(temp.path(), "write", false),
            )
            .await;
        assert!(!write_result.is_error, "{}", write_result.output);
        assert_eq!(
            std::fs::read_to_string(&file).expect("written file should read"),
            "replacement\n"
        );

        for result in [
            read_tool::ReadTool
                .prepare(read_tool::ReadInput {
                    file_path: "relative.txt".into(),
                    offset: None,
                    limit: None,
                })
                .await
                .err(),
            write_tool::WriteTool
                .prepare(write_tool::WriteInput {
                    file_path: "relative.txt".into(),
                    content: "x".into(),
                })
                .await
                .err(),
            edit_tool::EditTool
                .prepare(edit_tool::EditInput {
                    file_path: "relative.txt".into(),
                    old_string: "x".into(),
                    new_string: "y".into(),
                    replace_all: None,
                })
                .await
                .err(),
        ] {
            let error = result.expect("relative path should reject");
            assert!(error.is_error);
            assert_eq!(error.output, "file_path must be absolute: relative.txt");
        }
    }

    #[tokio::test]
    async fn edit_changed_file_reports_error_without_overwriting_new_content() {
        let temp = crate::test_support::TestTempDir::new("stale-edit");
        let file = temp.write("note.txt", "before\n");
        let prepared = edit_tool::EditTool
            .prepare(edit_tool::EditInput {
                file_path: file.display().to_string(),
                old_string: "before".into(),
                new_string: "after".into(),
                replace_all: None,
            })
            .await
            .expect("initial file should prepare");
        std::fs::write(&file, "changed\n").expect("fixture should change after preview");

        let result = edit_tool::EditTool
            .execute_prepared(
                prepared,
                crate::test_support::tool_context(temp.path(), "edit", false),
            )
            .await;
        assert!(result.is_error);
        assert_eq!(
            result.output.split_once(" in ").map(|(reason, _)| reason),
            Some("old_string not found")
        );
        assert_eq!(
            std::fs::read_to_string(&file).expect("changed file should read"),
            "changed\n"
        );
    }

    #[tokio::test]
    async fn write_creates_parents_and_returns_complete_diff_metadata() {
        let temp = crate::test_support::TestTempDir::new("write-diff");
        let file = temp.path().join("created/note.txt");
        let result = write_tool::WriteTool
            .execute_prepared(
                write_tool::WriteTool
                    .prepare(write_tool::WriteInput {
                        file_path: file.display().to_string(),
                        content: "alpha\nbeta\n".into(),
                    })
                    .await
                    .expect("absolute path should prepare"),
                crate::test_support::tool_context(temp.path(), "write", false),
            )
            .await;

        assert!(!result.is_error, "{}", result.output);
        assert_eq!(
            std::fs::read_to_string(&file).expect("created file should read"),
            "alpha\nbeta\n"
        );
        let metadata = result.metadata.expect("write should return diff metadata");
        assert_eq!(
            metadata.get("file_path"),
            Some(&serde_json::json!(file.display().to_string()))
        );
        let diff = metadata
            .get("diff")
            .and_then(|value| value.as_str())
            .expect("diff should be text");
        assert!(diff.starts_with("--- "));
        assert!(diff.contains("+alpha"));
        assert!(diff.contains("+beta"));
    }

    #[tokio::test]
    async fn view_image_requires_image_model() {
        let temp = crate::test_support::TestTempDir::new("view-image");
        let file = temp.write("image.PNG", b"png-bytes");
        let prepared = view_image_tool::ViewImageTool
            .prepare(view_image_tool::ViewImageInput {
                path: file.display().to_string(),
            })
            .await
            .expect("supported image should prepare");

        let result = view_image_tool::ViewImageTool
            .execute_prepared(
                prepared.clone(),
                crate::test_support::tool_context(temp.path(), "view_image", true),
            )
            .await;
        assert_eq!(
            result.output,
            format!("Loaded image: {} (9 bytes, image/png)", file.display())
        );
        assert!(!result.is_error);
        assert_eq!(
            result.extra_blocks,
            Some(vec![
                omini_domain::message::ContentBlock::from_base64_image(
                    "image/png".into(),
                    "cG5nLWJ5dGVz".into()
                )
            ])
        );

        let rejected = view_image_tool::ViewImageTool
            .execute_prepared(
                prepared,
                crate::test_support::tool_context(temp.path(), "view_image", false),
            )
            .await;
        assert!(rejected.is_error);
        assert_eq!(
            rejected.output,
            "view_image requires image input, but current model 'text-model' does not declare support for image input"
        );
    }
}
