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
mod start_screen;
mod status;
mod subagent_tool;
mod text;
mod theme;

use assistant::{build_assistant_text_lines, build_llm_summary_lines, build_proposed_plan_lines};
use messages::render_messages;
use scroll::{ScrollableLine, scrollable_lines};
use session_list::render_session_list;
use start_screen::render_start_screen;
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
    use crate::types::config::{ModelConfig, ThinkingEffort};
    use crate::types::display::DisplayPlan;
    use crate::types::events::{
        CommandKind, CommandSummary, PermissionPreview, ReadPermissionPreview, RuntimeToUiEvent,
        SessionSummary, SessionUsageSnapshot, ToolPauseKind, ToolPauseRequest, UserInputOption,
        UserInputPreview, UserInputQuestion,
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
    fn start_screen_renders_on_initial_empty_state() {
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = UiState::new();
        state.status_bar.model = "test-model".to_string();
        state.status_bar.active_provider = "test-provider".to_string();
        state.status_bar.thinking_effort = Some(ThinkingEffort::Medium);
        state.autocomplete.all_commands = vec![
            command_summary("help", CommandKind::Builtin),
            command_summary("commit-message", CommandKind::Skill),
        ];
        let now = Utc::now();
        state.startup_recent_sessions = vec![SessionSummary {
            id: "session-1".to_string(),
            title: "Fix flaky CI".to_string(),
            model: "test-model".to_string(),
            provider: "test-provider".to_string(),
            created_at: now,
            updated_at: now,
        }];

        terminal.draw(|frame| render(&mut state, frame)).unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("██████"));
        assert!(rendered.contains("test-model"));
        assert!(rendered.contains("medium"));
        assert!(rendered.contains("test-provider"));
        assert!(rendered.contains("Fix flaky CI"));
        assert!(rendered.contains("Recent Sessions"));
        assert!(rendered.contains("Startup Tip"));
        assert!(rendered.contains("/sessions"));
        assert_eq!(rendered.matches("skill").count(), 1);
        assert!(
            state
                .selectable_screen_lines
                .iter()
                .any(|line| line.text.contains("Welcome back!"))
        );
    }

    #[test]
    fn start_screen_is_hidden_after_empty_session_change() {
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = UiState::new();
        state.apply_session_changed(None, vec![], vec![], SessionUsageSnapshot::default());

        terminal.draw(|frame| render(&mut state, frame)).unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(!rendered.contains("██████"));
        assert!(!state.show_start_screen);
    }

    #[test]
    fn start_screen_is_hidden_while_help_drawer_is_open() {
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = UiState::new();
        state.open_help_drawer(vec![command_summary("help", CommandKind::Builtin)]);

        terminal.draw(|frame| render(&mut state, frame)).unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(!rendered.contains("██████"));
        assert!(!state.show_start_screen);
    }

    #[test]
    fn start_screen_keeps_normal_footer_and_input_layout() {
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = UiState::new();
        state.status_bar.model = "footer-model".to_string();

        terminal.draw(|frame| render(&mut state, frame)).unwrap();

        let buffer = terminal.backend().buffer();
        let prompt_idx = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .position(|cell| cell.symbol() == "❯")
            .expect("input prompt should render");
        let input_row = prompt_idx / 100;
        let input_col = prompt_idx % 100;
        assert_eq!(input_row, 21);
        assert_eq!(input_col, 0);

        let bottom_row = buffer
            .content()
            .chunks(100)
            .last()
            .expect("terminal has a bottom row")
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(bottom_row.contains("footer-model"));
    }

    #[test]
    fn start_screen_renders_in_tiny_terminal() {
        let backend = TestBackend::new(30, 6);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = UiState::new();

        terminal.draw(|frame| render(&mut state, frame)).unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("omini"));
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

    fn command_summary(name: &str, kind: CommandKind) -> CommandSummary {
        CommandSummary {
            name: name.to_string(),
            aliases: Vec::new(),
            description: String::new(),
            sort_weight: 0,
            kind,
            has_args: false,
            args_description: None,
        }
    }

    #[test]
    fn model_drawer_layout_leaves_gap_above_divider() {
        let backend = TestBackend::new(80, 18);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = UiState::new();
        state.interaction_step = Some(InteractionStep::ModelSelection {
            entries: vec![ModelSelectionEntry::Model {
                provider_key: "test".to_string(),
                model: ModelConfig {
                    id: "test-model".to_string(),
                    name: None,
                    limit: 1_000,
                    thinking: true,
                },
            }],
            selected: 0,
            thinking_idx: 0,
            active_provider: "test".to_string(),
            active_model: "test-model".to_string(),
        });

        terminal.draw(|frame| render(&mut state, frame)).unwrap();

        let area = Rect::new(0, 0, 80, 18);
        let drawer_height = interactions::interaction_drawer_height(&state, area)
            .expect("model drawer should have a height");
        let drawer_top = area.height - drawer_height;
        let messages_bottom = state.messages_area.y + state.messages_area.height;

        assert_eq!(messages_bottom + 1, drawer_top);
    }

    #[test]
    fn help_drawer_layout_leaves_gap_above_divider() {
        let backend = TestBackend::new(80, 18);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = UiState::new();
        state.help_drawer = Some(HelpDrawerState::new(Vec::new()));

        terminal.draw(|frame| render(&mut state, frame)).unwrap();

        let area = Rect::new(0, 0, 80, 18);
        let reserved_height = 3;
        let drawer_height = help_drawer::help_drawer_height(area)
            .min(area.height.saturating_sub(reserved_height).max(1));
        let drawer_top = area.height - drawer_height;
        let messages_bottom = state.messages_area.y + state.messages_area.height;

        assert_eq!(messages_bottom + 1, drawer_top);
    }

    #[test]
    fn permission_drawer_renders_in_tiny_terminal() {
        let backend = TestBackend::new(80, 4);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = UiState::new();
        state.apply_event(RuntimeToUiEvent::ToolPauseRequested(ToolPauseRequest {
            tool_use_id: "read-1".to_string(),
            preview_tool_use_id: None,
            tool_name: "read".to_string(),
            permission_source: None,
            source_session_id: None,
            source_agent_label: None,
            kind: ToolPauseKind::Permission(PermissionPreview::Read(ReadPermissionPreview {
                file_path: "Cargo.toml".to_string(),
            })),
        }));

        terminal.draw(|frame| render(&mut state, frame)).unwrap();
    }

    #[test]
    fn permission_drawer_keeps_pending_tool_visible_and_highlighted() {
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
        state.apply_event(RuntimeToUiEvent::ToolPauseRequested(ToolPauseRequest {
            tool_use_id: "read-1".to_string(),
            preview_tool_use_id: None,
            tool_name: "read".to_string(),
            permission_source: None,
            source_session_id: None,
            source_agent_label: None,
            kind: ToolPauseKind::Permission(PermissionPreview::Read(ReadPermissionPreview {
                file_path: "Cargo.toml".to_string(),
            })),
        }));

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
        assert!(rendered.contains("Waiting for permission"));
        assert!(rendered.contains("•"));
    }

    #[test]
    fn permission_drawer_layout_leaves_gap_above_divider() {
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
        state.apply_event(RuntimeToUiEvent::ToolPauseRequested(ToolPauseRequest {
            tool_use_id: "read-1".to_string(),
            preview_tool_use_id: None,
            tool_name: "read".to_string(),
            permission_source: None,
            source_session_id: None,
            source_agent_label: None,
            kind: ToolPauseKind::Permission(PermissionPreview::Read(ReadPermissionPreview {
                file_path: "Cargo.toml".to_string(),
            })),
        }));

        terminal.draw(|frame| render(&mut state, frame)).unwrap();

        let messages_bottom = state.messages_area.y + state.messages_area.height;
        let divider_top = state.permission_drawer_area.y.saturating_sub(1);
        assert_eq!(messages_bottom + 1, divider_top);
    }

    #[test]
    fn user_input_drawer_layout_leaves_gap_above_divider() {
        let backend = TestBackend::new(80, 18);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = UiState::new();
        state.apply_event(RuntimeToUiEvent::ToolPauseRequested(ToolPauseRequest {
            tool_use_id: "ask-1".to_string(),
            preview_tool_use_id: None,
            tool_name: "ask_user".to_string(),
            permission_source: None,
            source_session_id: None,
            source_agent_label: None,
            kind: ToolPauseKind::UserInput(UserInputPreview {
                questions: vec![UserInputQuestion {
                    id: "choice".to_string(),
                    header: "Choice".to_string(),
                    question: "Pick one".to_string(),
                    options: vec![UserInputOption {
                        label: "First".to_string(),
                        description: "Use the first option".to_string(),
                    }],
                }],
            }),
        }));

        terminal.draw(|frame| render(&mut state, frame)).unwrap();

        let messages_bottom = state.messages_area.y + state.messages_area.height;
        let divider_top = state.permission_drawer_area.y.saturating_sub(1);
        assert_eq!(messages_bottom + 1, divider_top);
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
