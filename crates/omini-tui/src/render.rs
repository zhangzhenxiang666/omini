use crate::state::{
    AgentCreateStep, AgentEditorField, AgentManagerState, AgentManagerView, AgentModelEntry,
    InteractionStep, ModelSelectionEntry, UiMessage, UiState,
};
use crate::types::events::{PermissionPreview, ToolPauseKind, ToolPauseRequest};
use crate::types::message::{ContentBlock, ToolUseBlock};
use crate::widgets::{display_path, render_tool};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Clear, Paragraph};
use std::path::Path;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

mod agents;
mod assistant;
mod autocomplete;
mod help_drawer;
mod input;
mod interactions;
mod layout;
mod messages;
mod permission_drawer;
mod plan_approval_drawer;
mod scroll;
mod session_list;
mod status;
mod subagent_tool;
mod text;
mod theme;

use assistant::{build_assistant_text_lines, build_llm_summary_lines, build_proposed_plan_lines};
use messages::render_messages;
use scroll::{ScrollableLine, scrollable_lines};
use session_list::render_session_list;
use subagent_tool::render_subagent_tool;
use text::{
    apply_text_selection_highlight, line_to_plain_text, line_width, pad_display_width,
    register_and_highlight_lines, styled_wrapped_display, styled_wrapped_text, truncate_str,
};
use theme::INPUT_BG;

pub fn render(state: &mut UiState, frame: &mut ratatui::Frame) {
    layout::render(state, frame);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::HelpDrawerState;
    use crate::types::config::ModelConfig;
    use crate::types::display::DisplayPlan;
    use crate::types::events::{
        PermissionPreview, ReadPermissionPreview, ToolPauseKind, ToolPauseRequest,
    };
    use crate::types::message::{Message, Role};
    use chrono::Utc;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    #[test]
    fn help_drawer_renders_in_tiny_terminal() {
        let backend = TestBackend::new(169, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = UiState::new();
        state.help_drawer = Some(HelpDrawerState::new(Vec::new()));

        terminal.draw(|frame| render(&mut state, frame)).unwrap();
    }

    #[test]
    fn model_drawer_renders_in_tiny_terminal() {
        let backend = TestBackend::new(80, 4);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = UiState::new();
        state.interaction_step = Some(InteractionStep::ModelSelection {
            entries: vec![ModelSelectionEntry::Model {
                provider_key: "test".to_string(),
                model: ModelConfig {
                    id: "tiny-model".to_string(),
                    name: None,
                    limit: 1_000,
                    thinking: true,
                },
            }],
            selected: 0,
            thinking_idx: 0,
            active_provider: "test".to_string(),
            active_model: "tiny-model".to_string(),
        });

        terminal.draw(|frame| render(&mut state, frame)).unwrap();
    }

    #[test]
    fn permission_drawer_renders_in_tiny_terminal() {
        let backend = TestBackend::new(80, 4);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = UiState::new();
        state.pending_tool_previews.insert(
            "read-1".to_string(),
            ToolPauseRequest {
                tool_use_id: "read-1".to_string(),
                preview_tool_use_id: None,
                tool_name: "read".to_string(),
                permission_source: None,
                source_session_id: None,
                source_agent_label: None,
                kind: ToolPauseKind::Permission(PermissionPreview::Read(ReadPermissionPreview {
                    file_path: "Cargo.toml".to_string(),
                })),
            },
        );

        terminal.draw(|frame| render(&mut state, frame)).unwrap();
    }

    #[test]
    fn permission_drawer_hides_pending_tool_loading_card() {
        let backend = TestBackend::new(80, 18);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = UiState::new();
        let mut input = std::collections::HashMap::new();
        input.insert("file_path".to_string(), serde_json::json!("Cargo.toml"));
        state.pending_assistant = Some(Message::new(
            Role::Assistant,
            vec![ContentBlock::ToolUse(ToolUseBlock {
                id: "read-1".to_string(),
                name: "read".to_string(),
                input,
            })],
        ));
        state.pending_tool_previews.insert(
            "read-1".to_string(),
            ToolPauseRequest {
                tool_use_id: "read-1".to_string(),
                preview_tool_use_id: None,
                tool_name: "read".to_string(),
                permission_source: None,
                source_session_id: None,
                source_agent_label: None,
                kind: ToolPauseKind::Permission(PermissionPreview::Read(ReadPermissionPreview {
                    file_path: "Cargo.toml".to_string(),
                })),
            },
        );

        terminal.draw(|frame| render(&mut state, frame)).unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Read File"));
        assert!(rendered.contains("Cargo.toml"));
        for frame in ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"] {
            assert!(!rendered.contains(frame));
        }
    }

    #[test]
    fn plan_approval_drawer_renders_in_tiny_terminal() {
        let backend = TestBackend::new(80, 4);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = UiState::new();
        state.plan_approval = Some(DisplayPlan {
            id: "20260522T000000Z-plan".to_string(),
            title: "Plan".to_string(),
            markdown: "# Plan\n\n- Step".to_string(),
            path: "/tmp/plan.md".into(),
            created_at: Utc::now(),
        });

        terminal.draw(|frame| render(&mut state, frame)).unwrap();
    }

    #[test]
    fn plan_approval_layout_leaves_gap_above_drawer() {
        let backend = TestBackend::new(80, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = UiState::new();
        state.plan_approval = Some(DisplayPlan {
            id: "20260522T000000Z-plan".to_string(),
            title: "Plan".to_string(),
            markdown: "# Plan\n\n- Step".to_string(),
            path: "/tmp/plan.md".into(),
            created_at: Utc::now(),
        });

        terminal.draw(|frame| render(&mut state, frame)).unwrap();

        let drawer_height =
            plan_approval_drawer::plan_approval_drawer_height(Rect::new(0, 0, 80, 12));
        let drawer_top = 12 - drawer_height;
        let messages_bottom = state.messages_area.y + state.messages_area.height;

        assert_eq!(messages_bottom + 1, drawer_top);
    }
}
