use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::types::message::ToolResultBlock;

pub mod bash_tool;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// 工具执行后的内部结果（比 ToolResultBlock 更通用）。
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
            output: self.output,
        }
    }
}

/// 工具作者看到的 trait —— execute 收到的是强类型的 Self::Input。
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
                let value = match serde_json::to_value(&input) {
                    Ok(v) => v,
                    Err(e) => return ToolResult::error(format!("Serialize input failed: {e}")),
                };
                let input: T::Input = match serde_json::from_value(value) {
                    Ok(i) => i,
                    Err(e) => return ToolResult::error(format!("Deserialize input failed: {e}")),
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

    pub async fn execute(&self, input: HashMap<String, Value>) -> ToolResult {
        (self.executor)(input).await
    }
}

/// 工具注册表。
#[derive(Default)]
pub struct ToolRegistry {
    tools: HashMap<String, RegisteredTool>,
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

    /// 返回所有工具定义（供 LLM API 使用）
    pub fn definitions(&self) -> Vec<&RegisteredTool> {
        self.tools.values().collect()
    }
}
