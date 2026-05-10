use crate::types::message::ToolResultBlock;
use crate::types::tool::ToolDefinition;
use async_trait::async_trait;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

pub mod bash_tool;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

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
}

impl ToolResult {
    pub fn ok(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            is_error: false,
        }
    }

    pub fn error(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            is_error: true,
        }
    }

    /// 关联 tool_use_id 转为 LLM API 需要的格式
    pub fn into_block(self, tool_use_id: &str) -> ToolResultBlock {
        ToolResultBlock {
            tool_use_id: tool_use_id.to_string(),
            is_error: self.is_error,
            content: self.output,
        }
    }
}

#[async_trait]
pub trait Tool: Send + Sync + 'static {
    /// 每个工具关联自己的输入参数结构体（需派生 JsonSchema + Deserialize）
    type Input: DeserializeOwned + JsonSchema + Send;

    fn name(&self) -> &str;
    fn description(&self) -> &str;

    /// 自动从 Self::Input 生成 JSON Schema
    fn input_schema(&self) -> Value {
        serde_json::to_value(schemars::schema_for!(Self::Input)).unwrap()
    }

    /// 执行工具，input 已是反序列化好的结构体
    async fn execute(&self, input: Self::Input) -> ToolResult;
}

/// 注册表中存的「已擦除类型」的工具。
pub struct RegisteredTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    executor: Box<dyn Fn(HashMap<String, Value>) -> BoxFuture<'static, ToolResult> + Send + Sync>,
}

impl RegisteredTool {
    pub fn new<T: Tool>(tool: T) -> Self {
        let name = tool.name().to_string();
        let description = tool.description().to_string();
        let input_schema = tool.input_schema();
        let tool = Arc::new(tool);
        let executor: Box<
            dyn Fn(HashMap<String, Value>) -> BoxFuture<'static, ToolResult> + Send + Sync,
        > = Box::new(move |input: HashMap<String, Value>| {
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
                tool.execute(input).await
            })
        });
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

    pub async fn execute(&self, input: HashMap<String, Value>) -> ToolResult {
        (self.executor)(input).await
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
    registry
}
