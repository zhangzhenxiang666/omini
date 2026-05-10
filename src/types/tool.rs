use serde::Serialize;
use serde_json::Value;

/// 工具定义，描述一个工具的名称、描述和输入 schema。
///
/// 由 `RegisteredTool::definition()` 生成，用于向 LLM API 注册可用工具。
#[derive(Debug, Clone, Serialize)]
pub struct ToolDefinition {
    /// 工具名称（LLM 通过此名称引用该工具）
    pub name: String,
    /// 工具描述（LLM 理解何时调用此工具的依据）
    pub description: String,
    /// 输入参数的 JSON Schema
    pub input_schema: Value,
}
