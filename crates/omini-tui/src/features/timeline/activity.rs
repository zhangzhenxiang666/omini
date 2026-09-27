use crate::features::tools::{ToolCategory, activity_summary_line, tool_category};
use omini_model::message::{ContentBlock, ToolResultBlock, ToolUseBlock};
use ratatui::text::Line;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssistantBlockKind {
    Thinking,
    AggregatedTool,
    BoundaryTool,
    VisibleText,
    ToolResult,
    Ignored,
}

/// 将助手块按时间线语义分类；权限状态不参与分类，保证审批队列不会改变消息分组。
pub fn classify_assistant_block(block: &ContentBlock) -> AssistantBlockKind {
    match block {
        ContentBlock::Thinking(_) => AssistantBlockKind::Thinking,
        ContentBlock::Text(text) if !text.text.trim().is_empty() => AssistantBlockKind::VisibleText,
        ContentBlock::Text(_) => AssistantBlockKind::Ignored,
        ContentBlock::ToolUse(tool_use) if is_activity_boundary_tool(tool_use) => {
            AssistantBlockKind::BoundaryTool
        }
        ContentBlock::ToolUse(_) => AssistantBlockKind::AggregatedTool,
        ContentBlock::ToolResult(_) => AssistantBlockKind::ToolResult,
        ContentBlock::Image(_) => AssistantBlockKind::Ignored,
    }
}

pub fn is_activity_boundary_tool(tool_use: &ToolUseBlock) -> bool {
    matches!(
        tool_use.name.as_str(),
        "spawn_agent" | "run_agent" | "edit" | "ask_user" | "write" | "todo_write"
    )
}

#[derive(Debug, Clone, Default)]
pub struct ActivityGroup {
    thinking_ms: Option<u64>,
    active_thinking_ms: Option<u64>,
    tool_counts: Vec<(ToolCategory, usize)>,
    last_tool: Option<Arc<ToolUseBlock>>,
    pub orphan_results: Vec<ToolResultBlock>,
}

impl ActivityGroup {
    pub fn is_empty(&self) -> bool {
        self.thinking_ms.is_none()
            && self.active_thinking_ms.is_none()
            && self.tool_counts.is_empty()
            && self.last_tool.is_none()
            && self.orphan_results.is_empty()
    }

    pub fn add_thinking(&mut self, duration_ms: Option<u64>) {
        if let Some(duration_ms) = duration_ms {
            self.thinking_ms = Some(
                self.thinking_ms
                    .unwrap_or_default()
                    .saturating_add(duration_ms),
            );
        }
    }

    pub fn set_active_thinking(&mut self, duration_ms: u64) {
        self.active_thinking_ms = Some(duration_ms);
    }

    pub fn add_tool(&mut self, tool_use: &ToolUseBlock) {
        // 检查点会复制活动组；共享工具输入，避免大参数随历史消息反复复制。
        self.last_tool = Some(Arc::new(tool_use.clone()));
        let category = tool_category(tool_use);
        if let Some((_, count)) = self
            .tool_counts
            .iter_mut()
            .find(|(existing, _)| *existing == category)
        {
            *count = count.saturating_add(1);
        } else {
            self.tool_counts.push((category, 1));
        }
    }

    pub fn last_tool(&self) -> Option<&ToolUseBlock> {
        self.last_tool.as_deref()
    }

    pub fn summary(&self, content_width: usize) -> Vec<Line<'static>> {
        let thinking_ms = match (self.thinking_ms, self.active_thinking_ms) {
            (Some(measured), Some(active)) => Some(measured.saturating_add(active)),
            (Some(measured), None) => Some(measured),
            (None, active) => active,
        };
        activity_summary_line(thinking_ms, &self.tool_counts, content_width)
            .into_iter()
            .collect()
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use std::collections::HashMap;

    #[test]
    fn visible_text_and_named_tools_are_boundaries() {
        let text = ContentBlock::from_text("answer".to_string());
        assert_eq!(
            classify_assistant_block(&text),
            AssistantBlockKind::VisibleText
        );

        for name in ["spawn_agent", "edit", "ask_user", "write", "todo_write"] {
            let tool =
                ContentBlock::from_tool_use(name.to_string(), name.to_string(), HashMap::new());
            assert_eq!(
                classify_assistant_block(&tool),
                AssistantBlockKind::BoundaryTool,
                "{name} must split activity groups"
            );
        }
    }

    #[test]
    fn blank_text_results_and_other_tools_do_not_create_boundaries() {
        let blank_text = ContentBlock::from_text(" \n ".to_string());
        assert_eq!(
            classify_assistant_block(&blank_text),
            AssistantBlockKind::Ignored
        );
        assert_eq!(
            classify_assistant_block(&ContentBlock::from_tool_use(
                "read-1".to_string(),
                "read".to_string(),
                HashMap::new(),
            )),
            AssistantBlockKind::AggregatedTool
        );
        assert_eq!(
            classify_assistant_block(&ContentBlock::from_tool_result(
                "read-1".to_string(),
                false,
                "contents".to_string(),
            )),
            AssistantBlockKind::ToolResult
        );
        assert_eq!(
            classify_assistant_block(&ContentBlock::from_thinking("hidden".to_string())),
            AssistantBlockKind::Thinking
        );
    }
}
