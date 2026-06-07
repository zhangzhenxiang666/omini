use crate::config::project::{ProjectDir, SessionDir};
use crate::permissions::{PermissionDecision, PermissionEngine};
use crate::skills::SkillRegistry;
use crate::subagents::{AgentRegistry, RuntimeSubagentRunner};
use crate::types::config::Settings;
use crate::types::events::{
    ActiveProfile, EngineToRuntimeEvent, PermissionPreview, PermissionSource, ToolPauseKind,
    ToolPauseRequest, ToolPauseResponse, UserInputPreview,
};
use crate::types::message::{ContentBlock, ToolResultBlock};
use crate::types::tool::ToolDefinition;
use async_trait::async_trait;
use schemars::JsonSchema;
use schemars::generate::SchemaSettings;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{Notify, mpsc, oneshot};

pub mod ask_user_tool;
pub mod bash_tool;
pub mod edit_tool;
pub mod read_tool;
pub mod search_tool;
pub mod skill_tool;
pub mod subagent_tool;
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

    /// 关联 tool_use_id 转为 LLM API 需要的格式
    pub fn into_block(self, tool_use_id: &str) -> ToolResultBlock {
        ToolResultBlock {
            tool_use_id: tool_use_id.to_string(),
            is_error: self.is_error,
            content: self.output,
            metadata: self.metadata,
        }
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
    pub settings: Option<Arc<Settings>>,
    pub tool_registry: Option<Arc<ToolRegistry>>,
    pub event_tx: mpsc::Sender<EngineToRuntimeEvent>,
    pub pending_tool_pauses: PendingToolPauses,
    pub permission_engine: Arc<PermissionEngine>,
    pub active_profile: ActiveProfile,
    pub cancelled: Arc<AtomicBool>,
    pub cancel_notify: Arc<Notify>,
    pub runtime: Option<Arc<ToolRuntimeContext>>,
}

#[derive(Clone)]
pub struct ToolRuntimeContext {
    pub session_id: String,
    pub run_id: Option<String>,
    pub session_type: String,
    pub agent_label: Option<String>,
    pub session_dir: SessionDir,
    pub subagent_registry: Arc<AgentRegistry>,
    pub skill_registry: Arc<SkillRegistry>,
    pub subagent_runner: Option<Arc<RuntimeSubagentRunner>>,
    pub project: ProjectDir,
}

impl std::fmt::Debug for ToolRuntimeContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolRuntimeContext")
            .field("session_id", &self.session_id)
            .field("run_id", &self.run_id)
            .field("session_type", &self.session_type)
            .field("agent_label", &self.agent_label)
            .field("session_dir", &self.session_dir)
            .field("subagent_registry", &self.subagent_registry)
            .field("skill_registry", &self.skill_registry)
            .field("project", &self.project)
            .finish_non_exhaustive()
    }
}

impl ToolExecutionContext {
    #[cfg(test)]
    pub fn test(tool_name: &str) -> Self {
        let (event_tx, _event_rx) = mpsc::channel(1);
        Self {
            tool_use_id: format!("test_{tool_name}"),
            pause_id: format!("test_{tool_name}"),
            tool_name: tool_name.to_string(),
            settings: None,
            tool_registry: None,
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
            source_session_id: self
                .runtime
                .as_ref()
                .map(|runtime| runtime.session_id.clone()),
            source_agent_label: self
                .runtime
                .as_ref()
                .and_then(|runtime| runtime.agent_label.clone()),
            kind: ToolPauseKind::Permission(preview),
        };

        if self
            .event_tx
            .send(EngineToRuntimeEvent::ToolPauseRequested(request))
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
            source_session_id: self
                .runtime
                .as_ref()
                .map(|runtime| runtime.session_id.clone()),
            source_agent_label: self
                .runtime
                .as_ref()
                .and_then(|runtime| runtime.agent_label.clone()),
            kind: ToolPauseKind::UserInput(preview),
        };

        if self
            .event_tx
            .send(EngineToRuntimeEvent::ToolPauseRequested(request))
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

fn normalize_tool_paths(tool_name: &str, raw_input: &mut Value, cwd: &Path) {
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
            *path = resolve_session_path(cwd, raw).display().to_string();
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

fn resolve_session_path(cwd: &Path, raw: &str) -> PathBuf {
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

pub(crate) fn sanitize_tool_schema(value: &mut Value) {
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
                    if let Some(settings) = ctx.settings.as_deref() {
                        normalize_tool_paths(tool.name(), &mut raw_input, &settings.cwd);
                    }
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

/// 创建默认的工具注册表，注册所有内置工具。
///
/// 当需要集成所有 tools/ 中定义的工具时，调用此函数即可。
pub fn create_default_registry() -> ToolRegistry {
    create_registry_with_allowed(None, false)
}

pub fn create_main_registry() -> ToolRegistry {
    create_registry_with_allowed(None, true)
}

pub fn create_subagent_registry(allowed_tools: &[String]) -> ToolRegistry {
    create_registry_with_allowed(Some(allowed_tools), false)
}

pub fn create_subagent_registry_from_parent(
    parent: &ToolRegistry,
    allow: Option<&[String]>,
    deny: &[String],
) -> Result<(ToolRegistry, Vec<String>), String> {
    let mut warnings = Vec::new();
    let parent_names: HashSet<String> = parent.tool_names().into_iter().collect();
    let deny_names: HashSet<&str> = deny.iter().map(String::as_str).collect();
    let mut selected = Vec::new();

    match allow {
        Some(allow) => {
            for name in allow {
                if name == "subagent" {
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
                    .filter(|name| name.as_str() != "subagent")
                    .filter(|name| !deny_names.contains(name.as_str()))
                    .cloned(),
            );
        }
    }

    for name in deny {
        if name != "subagent" && !parent_names.contains(name) {
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
                "subagent tool policy leaves no available tools: {}",
                warnings.join("; ")
            ));
        }
        return Err("subagent tool policy leaves no available tools".to_string());
    }

    Ok((parent.filtered(&selected), warnings))
}

pub fn inherited_subagent_tool_names() -> Vec<String> {
    create_subagent_registry(&[
        "ask_user".to_string(),
        "bash".to_string(),
        "search".to_string(),
        "skill".to_string(),
        "read".to_string(),
        "view_image".to_string(),
        "edit".to_string(),
        "write".to_string(),
    ])
    .tool_names()
}

fn create_registry_with_allowed(
    allowed: Option<&[String]>,
    include_subagent: bool,
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
    if include_subagent && tool_allowed(allowed, "todo_write") {
        registry.register(todo_tool::TodoWriteTool);
    }
    if include_subagent && tool_allowed(allowed, "subagent") {
        registry.register(subagent_tool::SubagentTool);
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
        "subagent" => 9,
        _ => 100,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_search_default_path_uses_session_cwd() {
        let cwd = Path::new("/repo");
        let mut input = serde_json::json!({ "query": "needle" });

        normalize_tool_paths("search", &mut input, cwd);

        assert_eq!(input["path"], serde_json::json!("/repo"));
    }

    #[test]
    fn normalize_bash_relative_workdir_uses_session_cwd() {
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
    fn normalize_file_tool_relative_file_path_uses_session_cwd() {
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
    fn subagent_registry_inherits_parent_tools_without_subagent() {
        let parent = create_main_registry();

        let (child, warnings) =
            create_subagent_registry_from_parent(&parent, None, &[]).expect("policy should work");

        assert!(warnings.is_empty());
        assert!(child.contains("search"));
        assert!(child.contains("read"));
        assert!(child.contains("write"));
        assert!(!child.contains("subagent"));
    }

    #[test]
    fn subagent_registry_applies_allow_then_deny() {
        let parent = create_main_registry();
        let allow = vec![
            "read".to_string(),
            "search".to_string(),
            "write".to_string(),
            "subagent".to_string(),
        ];
        let deny = vec!["write".to_string()];

        let (child, warnings) = create_subagent_registry_from_parent(&parent, Some(&allow), &deny)
            .expect("policy should leave read available");

        assert!(warnings.is_empty());
        assert!(child.contains("search"));
        assert!(child.contains("read"));
        assert!(!child.contains("write"));
        assert!(!child.contains("subagent"));
    }

    #[test]
    fn main_registry_contains_search() {
        let registry = create_main_registry();

        assert!(registry.contains("search"));
    }

    #[test]
    fn tool_result_ok_and_error_have_no_extra_blocks_by_default() {
        let ok = ToolResult::ok("done");
        let error = ToolResult::error("failed");

        assert!(ok.extra_blocks.is_none());
        assert!(error.extra_blocks.is_none());
    }

    #[test]
    fn view_image_definition_is_always_exposed() {
        let registry = create_main_registry();

        assert!(
            registry
                .definitions()
                .iter()
                .any(|definition| definition.name == "view_image")
        );
    }

    #[test]
    fn main_registry_contains_todo_write_schema() {
        let definition = create_main_registry()
            .definitions()
            .into_iter()
            .find(|definition| definition.name == "todo_write")
            .expect("todo_write should be registered");

        let item_properties = definition
            .input_schema
            .get("properties")
            .and_then(|properties| properties.get("todos"))
            .and_then(|todos| todos.get("items"))
            .and_then(|items| items.get("properties"))
            .expect("todo_write items should have properties");

        assert!(item_properties.get("content").is_some());
        assert!(item_properties.get("status").is_some());
        assert!(item_properties.get("step").is_none());
    }

    #[test]
    fn tool_definitions_prioritize_search_over_bash() {
        let names: Vec<_> = create_main_registry()
            .definitions()
            .into_iter()
            .map(|definition| definition.name)
            .collect();

        let search = names.iter().position(|name| name == "search").unwrap();
        let bash = names.iter().position(|name| name == "bash").unwrap();
        assert!(search < bash, "{names:?}");
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
}
