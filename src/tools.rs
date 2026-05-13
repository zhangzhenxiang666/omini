use crate::types::events::{
    EngineToRuntimeEvent, PermissionPreview, ToolPauseKind, ToolPauseRequest, ToolPauseResponse,
};
use crate::types::message::ToolResultBlock;
use crate::types::tool::ToolDefinition;
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};

pub mod bash_tool;
pub mod edit_tool;
pub mod read_tool;
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

/// 工具执行后的内部结果
#[derive(Debug, Clone)]
pub struct ToolResult {
    pub output: String,
    pub is_error: bool,
    pub metadata: Option<Map<String, Value>>,
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
        }
    }

    pub fn error(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            is_error: true,
            metadata: None,
        }
    }

    pub fn with_metadata(mut self, metadata: Map<String, Value>) -> Self {
        self.metadata = Some(metadata);
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
}

pub type PendingToolPauses = Arc<Mutex<HashMap<String, PendingToolPause>>>;

#[derive(Debug)]
pub enum PendingToolPause {
    Permission(oneshot::Sender<ToolPauseResponse>),
    UserInput(oneshot::Sender<ToolPauseResponse>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionPolicyDecision {
    AutoAllow,
    AskUser,
    AutoDeny,
}

pub trait PermissionPolicy: Send + Sync + 'static {
    fn decide(&self, tool_name: &str, preview: &PermissionPreview) -> PermissionPolicyDecision;
}

#[derive(Debug, Default)]
pub struct DefaultPermissionPolicy;

impl PermissionPolicy for DefaultPermissionPolicy {
    fn decide(&self, tool_name: &str, _preview: &PermissionPreview) -> PermissionPolicyDecision {
        match tool_name {
            "read" => PermissionPolicyDecision::AutoAllow,
            "bash" | "edit" | "write" => PermissionPolicyDecision::AskUser,
            _ => PermissionPolicyDecision::AskUser,
        }
    }
}

#[derive(Clone)]
pub struct ToolExecutionContext {
    pub tool_use_id: String,
    pub tool_name: String,
    pub event_tx: mpsc::Sender<EngineToRuntimeEvent>,
    pub pending_tool_pauses: PendingToolPauses,
    pub permission_policy: Arc<dyn PermissionPolicy>,
    pub cancelled: Arc<AtomicBool>,
}

impl ToolExecutionContext {
    pub async fn request_permission(&self, preview: PermissionPreview) -> ToolPauseResponse {
        if self.cancelled.load(Ordering::Relaxed) {
            return ToolPauseResponse::Cancelled;
        }

        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self
                .pending_tool_pauses
                .lock()
                .expect("pending tool pause mutex poisoned");
            if pending.contains_key(&self.tool_use_id) {
                return ToolPauseResponse::Permission { approved: false };
            }
            pending.insert(self.tool_use_id.clone(), PendingToolPause::Permission(tx));
        }

        let request = ToolPauseRequest {
            tool_use_id: self.tool_use_id.clone(),
            tool_name: self.tool_name.clone(),
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

    fn remove_pending_pause(&self) {
        let mut pending = self
            .pending_tool_pauses
            .lock()
            .expect("pending tool pause mutex poisoned");
        pending.remove(&self.tool_use_id);
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
        serde_json::to_value(schemars::schema_for!(Self::Input)).unwrap()
    }

    /// 预检查工具调用，生成内部强类型执行计划。
    async fn prepare(&self, input: Self::Input) -> Result<Self::Prepared, ToolResult>;

    /// 生成对外可序列化的权限预览。返回 None 表示无需权限审批。
    fn permission_preview(&self, _prepared: &Self::Prepared) -> Option<PermissionPreview> {
        None
    }

    /// 执行已经预检查过的工具计划。
    async fn execute_prepared(&self, prepared: Self::Prepared) -> ToolResult;
}

/// 注册表中存的「已擦除类型」的工具。
pub struct RegisteredTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    executor: Box<ToolExecutor>,
}

impl RegisteredTool {
    pub fn new<T: Tool>(tool: T) -> Self {
        let name = tool.name().to_string();
        let description = tool.description().to_string();
        let input_schema = tool.input_schema();
        let tool = Arc::new(tool);
        let executor: Box<ToolExecutor> = Box::new(
            move |input: HashMap<String, Value>, ctx: ToolExecutionContext| {
                let tool = Arc::clone(&tool);
                Box::pin(async move {
                    let value = Value::Object(input.into_iter().collect());
                    let input: T::Input = match serde_json::from_value(value) {
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

                    if let Some(preview) = tool.permission_preview(&prepared) {
                        match ctx.permission_policy.decide(tool.name(), &preview) {
                            PermissionPolicyDecision::AutoAllow => {}
                            PermissionPolicyDecision::AutoDeny => {
                                return ToolResult::error(format!(
                                    "Permission denied for tool: {}",
                                    tool.name()
                                ));
                            }
                            PermissionPolicyDecision::AskUser => {
                                match ctx.request_permission(preview).await {
                                    ToolPauseResponse::Permission { approved: true } => {}
                                    ToolPauseResponse::Permission { approved: false } => {
                                        return ToolResult::error(format!(
                                            "Permission denied for tool: {}",
                                            tool.name()
                                        ));
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
                    }

                    tool.execute_prepared(prepared).await
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
        self.tools.values().map(|t| t.definition()).collect()
    }
}

/// 创建默认的工具注册表，注册所有内置工具。
///
/// 当需要集成所有 tools/ 中定义的工具时，调用此函数即可。
pub fn create_default_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(bash_tool::BashTool);
    registry.register(read_tool::ReadTool);
    registry.register(edit_tool::EditTool);
    registry.register(write_tool::WriteTool);
    registry
}
