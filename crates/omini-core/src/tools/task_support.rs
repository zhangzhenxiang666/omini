use super::ToolResult;

/// 校验任务 ID 并去除首尾空白。
pub(super) fn normalize_task_id(task_id: &str) -> Result<String, ToolResult> {
    let task_id = task_id.trim();
    if task_id.is_empty() {
        return Err(ToolResult::error("task_id must not be empty"));
    }
    Ok(task_id.to_string())
}
