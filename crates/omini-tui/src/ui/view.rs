use crate::ui::prelude::*;
pub fn render(state: &AppState, frame: &mut ratatui::Frame) -> crate::ui::context::FrameState {
    let mut context = crate::ui::context::ViewContext::new(state);
    layout::render(&mut context, frame);
    context.finish()
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::app::event::{
        CommandKind, CommandSummary, PermissionPreview, ReadPermissionPreview, RuntimeToUiEvent,
        ThreadSummary, ThreadUsageSnapshot, ToolPauseKind, ToolPauseRequest, UserInputOption,
        UserInputPreview, UserInputQuestion,
    };
    use crate::client::catalog::{ModelConfig, ThinkingEffort};
    use crate::features::help::state::HelpDrawerState;
    use chrono::Utc;
    use omini_domain::conversation::ProposedPlan;
    use omini_model::message::{Message, Role};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;

    #[test]
    fn page_uses_terminal_background_and_keeps_input_panel() {
        for (width, height) in [(120, 36), (80, 24)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let mut state = AppState::new();
            terminal
                .draw(|frame| crate::app::draw(&mut state, frame))
                .unwrap();
            let buffer = terminal.backend().buffer();
            assert_eq!(buffer[(0, 0)].bg, Color::Reset);
            assert_eq!(buffer[(width - 1, 0)].bg, Color::Reset);
            let input_y = (1..height - 1)
                .find(|y| buffer[(0, *y)].symbol() == "❯")
                .unwrap();
            assert_eq!(buffer[(width - 1, input_y)].bg, crate::ui::theme::PANEL);
        }
    }

    /// 两种终端尺寸均在消息区下方显示临时状态，保持输入与子会话活动可用。
    #[test]
    fn background_wait_rendering() {
        use omini_domain::task::TaskStatus;
        for (width, height) in [(120, 36), (80, 24)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let mut state = AppState::new();
            state.start.show_start_screen = false;
            state.composer.input = "next request".into();
            state.track_background_task("agent".into(), TaskStatus::Running);
            state.track_background_task("bash".into(), TaskStatus::Cancelling);
            terminal
                .draw(|frame| crate::app::draw(&mut state, frame))
                .unwrap();
            let hint = state
                .geometry
                .selectable_screen_lines
                .iter()
                .find(|line| {
                    line.text
                        .contains("Waiting for 2 background tasks to finish")
                })
                .unwrap();
            assert!(hint.row >= state.geometry.messages_area.bottom());
            let input_row = (1..height - 1)
                .find(|y| terminal.backend().buffer()[(0, *y)].symbol() == "❯")
                .unwrap();
            assert_eq!(input_row - 1, hint.row + 1);
            let screen: String = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(screen.contains("next request"));
            assert!(state.sessions.views["main"].messages.is_empty());
            assert!(state.sessions.views["main"].run_timer.is_none());

            state.apply_event(RuntimeToUiEvent::RunStarted);
            terminal
                .draw(|frame| crate::app::draw(&mut state, frame))
                .unwrap();
            assert!(
                state
                    .geometry
                    .selectable_screen_lines
                    .iter()
                    .all(|line| !line.text.contains("background tasks to finish"))
            );
            state.apply_event(RuntimeToUiEvent::RunFinished);
            state.track_background_task("bash".into(), TaskStatus::Cancelled);
            terminal
                .draw(|frame| crate::app::draw(&mut state, frame))
                .unwrap();
            assert!(state.geometry.selectable_screen_lines.iter().any(|line| {
                line.text
                    .contains("Waiting for 1 background task to finish")
            }));

            // 查看子会话时应呈现子任务自身的活动状态，不显示主会话的后台提示。
            state.start_run_timer();
            let child_timer = state.sessions.views["main"].run_timer.take();
            state.sessions.active_session_task_id = Some("agent".into());
            state.sessions.views.insert(
                "agent".into(),
                crate::app::state::SessionState {
                    agent_status: crate::features::sessions::model::AgentStatus::Thinking,
                    run_timer: child_timer,
                    ..crate::app::state::SessionState::default()
                },
            );
            terminal
                .draw(|frame| crate::app::draw(&mut state, frame))
                .unwrap();
            assert!(
                state
                    .geometry
                    .selectable_screen_lines
                    .iter()
                    .all(|line| !line.text.contains("background task to finish"))
            );
            assert!(
                state
                    .geometry
                    .selectable_screen_lines
                    .iter()
                    .any(|line| line.text.contains("Thinking"))
            );
            state.sessions.views["agent"].agent_status =
                crate::features::sessions::model::AgentStatus::Idle;
            state.track_background_task("other".into(), TaskStatus::Running);
            terminal
                .draw(|frame| crate::app::draw(&mut state, frame))
                .unwrap();
            assert!(state.geometry.selectable_screen_lines.iter().any(|line| {
                line.text
                    .contains("Waiting for 1 background task to finish")
            }));
            state.sessions.views["main"].agent_status =
                crate::features::sessions::model::AgentStatus::Thinking;
            state.sessions.views["main"].main_query_active = true;
            terminal
                .draw(|frame| crate::app::draw(&mut state, frame))
                .unwrap();
            assert!(
                state
                    .geometry
                    .selectable_screen_lines
                    .iter()
                    .all(|line| { !line.text.contains("background task to finish") })
            );
            state.sessions.views["main"].agent_status =
                crate::features::sessions::model::AgentStatus::Idle;
            state.sessions.views["main"].main_query_active = false;
            state.sessions.active_session_task_id = None;
            state.track_background_task("agent".into(), TaskStatus::Completed);
            state.track_background_task("other".into(), TaskStatus::Completed);
            terminal
                .draw(|frame| crate::app::draw(&mut state, frame))
                .unwrap();
            assert!(
                state
                    .geometry
                    .selectable_screen_lines
                    .iter()
                    .all(|line| !line.text.contains("Waiting for"))
            );
        }
    }

    #[test]
    fn help_drawer_renders_in_tiny_terminal() {
        let backend = TestBackend::new(169, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new();
        state.dialogs.help_drawer = Some(HelpDrawerState::new(Vec::new()));

        terminal
            .draw(|frame| crate::app::draw(&mut state, frame))
            .unwrap();
    }

    #[test]
    fn start_screen_renders_on_initial_empty_state() {
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new();
        state.project.status_bar.model = "test-model".to_string();
        state.project.status_bar.active_provider = "test-provider".to_string();
        state.project.status_bar.thinking_effort = Some(ThinkingEffort::Medium);
        state.composer.autocomplete.all_commands = vec![
            command_summary("help", CommandKind::Builtin),
            command_summary("commit-message", CommandKind::Skill),
        ];
        let now = Utc::now();
        state.start.startup_recent_threads = vec![ThreadSummary {
            id: "thread-1".to_string(),
            title: "Fix flaky CI".to_string(),
            model: "test-model".to_string(),
            provider: "test-provider".to_string(),
            created_at: now,
            updated_at: now,
            runtime_state: None,
        }];

        terminal
            .draw(|frame| crate::app::draw(&mut state, frame))
            .unwrap();

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
        assert!(rendered.contains("Fix flaky CI"));
        assert!(rendered.contains("Recent Sessions"));
        assert!(rendered.contains("Startup Tip"));
        assert!(rendered.contains("/sessions"));
        assert!(
            state
                .geometry
                .selectable_screen_lines
                .iter()
                .any(|line| line.text.contains("Welcome back!"))
        );
    }

    #[test]
    fn thread_picker_renders_tail_runtime_state_markers() {
        let backend = TestBackend::new(60, 14);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new();
        let now = Utc::now();
        let threads = vec![
            thread_summary("stored", "Stored only", None, now),
            thread_summary(
                "idle",
                "Idle loaded",
                Some(omini_protocol::ThreadRuntimeState::Idle),
                now,
            ),
            thread_summary(
                "thinking",
                "Thinking loaded",
                Some(omini_protocol::ThreadRuntimeState::Thinking),
                now,
            ),
            thread_summary(
                "working",
                "Working loaded",
                Some(omini_protocol::ThreadRuntimeState::Working),
                now,
            ),
            thread_summary(
                "waiting",
                "Waiting loaded",
                Some(omini_protocol::ThreadRuntimeState::Waiting),
                now,
            ),
            thread_summary(
                "compacting",
                "Compacting loaded",
                Some(omini_protocol::ThreadRuntimeState::Compacting),
                now,
            ),
        ];
        state.dialogs.interaction_step = Some(InteractionStep::Thread {
            threads: threads.clone(),
            all_threads: threads,
            search: String::new(),
            selected: 0,
        });

        terminal
            .draw(|frame| crate::app::draw(&mut state, frame))
            .unwrap();

        let buffer = terminal.backend().buffer();
        assert!(!buffer.content().iter().any(|cell| cell.symbol() == "○"));
        let dot_colors = buffer
            .content()
            .iter()
            .filter(|cell| cell.symbol() == "●")
            .map(|cell| cell.fg)
            .collect::<Vec<_>>();
        for color in [
            crate::ui::theme::SUCCESS,
            crate::ui::theme::RUNNING,
            crate::ui::theme::WAITING,
            crate::ui::theme::SECONDARY,
        ] {
            assert!(
                dot_colors.contains(&color),
                "missing status color {color:?}"
            );
        }
        let width = buffer.area.width as usize;
        for (idx, cell) in buffer.content().iter().enumerate() {
            if cell.symbol() == "●" && dot_colors.contains(&cell.fg) {
                assert_ne!(
                    idx % width,
                    width - 1,
                    "status marker should not touch row edge"
                );
            }
        }
    }

    #[test]
    fn thread_picker_truncates_titles_before_tail_status_column() {
        let backend = TestBackend::new(34, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new();
        let now = Utc::now();
        let long_title = "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz";
        let threads = vec![thread_summary(
            "working",
            long_title,
            Some(omini_protocol::ThreadRuntimeState::Working),
            now,
        )];
        state.dialogs.interaction_step = Some(InteractionStep::Thread {
            threads: threads.clone(),
            all_threads: threads,
            search: String::new(),
            selected: 0,
        });

        terminal
            .draw(|frame| crate::app::draw(&mut state, frame))
            .unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("●"));
        assert!(rendered.contains("..."));
        assert!(!rendered.contains(long_title));
    }

    #[test]
    fn start_screen_is_hidden_after_empty_thread_change() {
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new();
        state.apply_thread_snapshot(None, vec![], vec![], ThreadUsageSnapshot::default());

        terminal
            .draw(|frame| crate::app::draw(&mut state, frame))
            .unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(!rendered.contains("██████"));
        assert!(!state.start.show_start_screen);
    }

    #[test]
    fn help_drawer_keeps_start_screen_context() {
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new();
        state.open_help_drawer(vec![command_summary("help", CommandKind::Builtin)]);

        terminal
            .draw(|frame| crate::app::draw(&mut state, frame))
            .unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        let message_y = state.geometry.messages_area.y as usize;
        let first_message_row = terminal.backend().buffer().content()
            [message_y * 100..(message_y + 1) * 100]
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(first_message_row.contains("omini"), "{rendered}");
        assert!(state.start.show_start_screen);
    }

    #[test]
    fn start_screen_keeps_normal_footer_and_input_layout() {
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new();
        state.project.status_bar.model = "footer-model".to_string();

        terminal
            .draw(|frame| crate::app::draw(&mut state, frame))
            .unwrap();

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
        let input_area_top = (input_row - 1) as u16;
        let messages_bottom = state.geometry.messages_area.y + state.geometry.messages_area.height;
        assert_eq!(input_area_top - messages_bottom, 1);

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
    fn footer_session_layout() {
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new();
        state.start.show_start_screen = false;
        state.project.status_bar.model = "footer-model".to_string();
        state.sessions.subagent_order.push("task-1".to_string());
        state.sessions.subagents.insert(
            "thread-1".to_string(),
            crate::features::sessions::model::SubagentNode {
                task_id: "task-1".to_string(),
                thread_id: "thread-1".to_string(),
                parent_thread_id: "main-thread".to_string(),
                spawn_tool_use_id: "tool-1".to_string(),
                agent_label: "Explore".to_string(),
                title: "Inspect project".to_string(),
                execution_mode: crate::app::event::AgentTaskExecutionMode::Background,
                status: omini_domain::task::TaskStatus::Running,
                duration: None,
                started_at: Utc::now(),
                messages: Vec::new(),
            },
        );
        state.sessions.views.insert(
            "task-1".to_string(),
            crate::app::state::SessionState::default(),
        );

        terminal
            .draw(|frame| crate::app::draw(&mut state, frame))
            .unwrap();

        let rows = terminal
            .backend()
            .buffer()
            .content()
            .chunks(100)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>();
        let prompt_row = rows
            .iter()
            .position(|row| row.contains("❯"))
            .expect("input prompt should render");
        let footer_row = rows
            .iter()
            .position(|row| row.contains("footer-model"))
            .expect("footer should render");
        let main_row = rows
            .iter()
            .position(|row| row.contains("main"))
            .expect("main session should render");

        assert_eq!(footer_row, prompt_row + 2);
        assert_eq!(main_row, footer_row + 2);
    }

    #[test]
    fn active_status_uses_one_row_and_idle_status_uses_none() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new();
        terminal
            .draw(|frame| crate::app::draw(&mut state, frame))
            .unwrap();
        let idle_messages_height = state.geometry.messages_area.height;

        state.sessions.views["main"].agent_status =
            crate::features::sessions::model::AgentStatus::Thinking;
        state.start_run_timer();
        terminal
            .draw(|frame| crate::app::draw(&mut state, frame))
            .unwrap();

        assert_eq!(
            state.geometry.messages_area.height,
            idle_messages_height - 1
        );
        let status_row = terminal
            .backend()
            .buffer()
            .content()
            .chunks(80)
            .position(|row| {
                row.iter()
                    .map(|cell| cell.symbol())
                    .collect::<String>()
                    .contains("esc to interrupt")
            })
            .expect("active status should render") as u16;
        let messages_bottom = state.geometry.messages_area.y + state.geometry.messages_area.height;
        assert_eq!(status_row - messages_bottom, 1);
        let input_row = (1..24)
            .find(|y| terminal.backend().buffer()[(0, *y)].symbol() == "❯")
            .unwrap();
        assert_eq!(input_row - 1, status_row + 1);
    }

    #[test]
    fn start_screen_renders_in_tiny_terminal() {
        let backend = TestBackend::new(30, 6);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new();

        terminal
            .draw(|frame| crate::app::draw(&mut state, frame))
            .unwrap();

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
        let mut state = AppState::new();
        state.dialogs.interaction_step = Some(InteractionStep::ModelSelection {
            entries: vec![ModelSelectionEntry::Model {
                provider_key: "test".to_string(),
                model: ModelConfig {
                    id: "tiny-model".to_string(),
                    name: None,
                    limit: 1_000,
                    thinking: true,
                    input_modalities: None,
                    extra_body: None,
                    extra_headers: None,
                },
            }],
            selected: 0,
            thinking_idx: 0,
            active_provider: "test".to_string(),
            active_model: "tiny-model".to_string(),
        });

        terminal
            .draw(|frame| crate::app::draw(&mut state, frame))
            .unwrap();
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

    fn thread_summary(
        id: &str,
        title: &str,
        runtime_state: Option<omini_protocol::ThreadRuntimeState>,
        now: chrono::DateTime<Utc>,
    ) -> ThreadSummary {
        ThreadSummary {
            id: id.to_string(),
            title: title.to_string(),
            model: "test-model".to_string(),
            provider: "test-provider".to_string(),
            created_at: now,
            updated_at: now,
            runtime_state: runtime_state.map(Into::into),
        }
    }

    #[test]
    fn model_drawer_layout_leaves_gap_above_divider() {
        let backend = TestBackend::new(80, 18);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new();
        state.dialogs.interaction_step = Some(InteractionStep::ModelSelection {
            entries: vec![ModelSelectionEntry::Model {
                provider_key: "test".to_string(),
                model: ModelConfig {
                    id: "test-model".to_string(),
                    name: None,
                    limit: 1_000,
                    thinking: true,
                    input_modalities: None,
                    extra_body: None,
                    extra_headers: None,
                },
            }],
            selected: 0,
            thinking_idx: 0,
            active_provider: "test".to_string(),
            active_model: "test-model".to_string(),
        });

        terminal
            .draw(|frame| crate::app::draw(&mut state, frame))
            .unwrap();

        let area = Rect::new(0, 0, 80, 18);
        let drawer_height = interactions::interaction_drawer_height(
            &crate::ui::context::ViewContext::new(&state),
            area,
        )
        .expect("model drawer should have a height");
        let drawer_top = area.height - drawer_height;
        let messages_bottom = state.geometry.messages_area.y + state.geometry.messages_area.height;

        assert_eq!(messages_bottom + 1, drawer_top);
    }

    #[test]
    fn help_drawer_layout_leaves_gap_above_divider() {
        let backend = TestBackend::new(80, 18);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new();
        state.dialogs.help_drawer = Some(HelpDrawerState::new(Vec::new()));

        terminal
            .draw(|frame| crate::app::draw(&mut state, frame))
            .unwrap();

        let area = Rect::new(0, 0, 80, 18);
        let reserved_height = 3;
        let drawer_height = help_drawer::help_drawer_height(area)
            .min(area.height.saturating_sub(reserved_height).max(1));
        let drawer_top = area.height - drawer_height;
        let messages_bottom = state.geometry.messages_area.y + state.geometry.messages_area.height;

        assert_eq!(messages_bottom + 1, drawer_top);
    }

    #[test]
    fn permission_drawer_renders_in_tiny_terminal() {
        let backend = TestBackend::new(80, 4);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new();
        state.apply_event(RuntimeToUiEvent::ToolPauseRequested(ToolPauseRequest {
            tool_use_id: "read-1".to_string(),
            preview_tool_use_id: None,
            tool_name: "read".to_string(),
            permission_source: None,
            source_thread_id: None,
            source_agent_label: None,
            kind: ToolPauseKind::Permission(PermissionPreview::Read(ReadPermissionPreview {
                file_path: "Cargo.toml".to_string(),
            })),
        }));

        terminal
            .draw(|frame| crate::app::draw(&mut state, frame))
            .unwrap();
    }

    #[test]
    fn permission_drawer_keeps_pending_tool_visible_and_highlighted() {
        let backend = TestBackend::new(80, 18);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new();
        let mut input = std::collections::HashMap::new();
        input.insert("file_path".to_string(), serde_json::json!("Cargo.toml"));
        state.sessions.views["main"].pending_assistant = Some(
            Message::new(
                Role::Assistant,
                vec![ContentBlock::ToolUse(ToolUseBlock {
                    id: "read-1".to_string(),
                    name: "read".to_string(),
                    input,
                })],
            )
            .into(),
        );
        state.apply_event(RuntimeToUiEvent::ToolPauseRequested(ToolPauseRequest {
            tool_use_id: "read-1".to_string(),
            preview_tool_use_id: None,
            tool_name: "read".to_string(),
            permission_source: None,
            source_thread_id: None,
            source_agent_label: None,
            kind: ToolPauseKind::Permission(PermissionPreview::Read(ReadPermissionPreview {
                file_path: "Cargo.toml".to_string(),
            })),
        }));

        terminal
            .draw(|frame| crate::app::draw(&mut state, frame))
            .unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("Read File"));
        assert!(rendered.contains("Cargo.toml"));
        assert!(
            state
                .geometry
                .selectable_screen_lines
                .iter()
                .any(|line| line.text.contains("1. 允许"))
        );
    }

    #[test]
    fn permission_queue_only_changes_drawer_not_timeline_order() {
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new();
        state.sessions.views["main"].pending_assistant = Some(
            Message::new(
                Role::Assistant,
                vec![
                    ContentBlock::from_tool_use(
                        "read-1".to_string(),
                        "read".to_string(),
                        std::collections::HashMap::from([(
                            "file_path".to_string(),
                            serde_json::json!("first.txt"),
                        )]),
                    ),
                    ContentBlock::from_tool_use(
                        "read-2".to_string(),
                        "read".to_string(),
                        std::collections::HashMap::from([(
                            "file_path".to_string(),
                            serde_json::json!("second.txt"),
                        )]),
                    ),
                ],
            )
            .into(),
        );
        let request = |tool_use_id: &str, file_path: &str| ToolPauseRequest {
            tool_use_id: tool_use_id.to_string(),
            preview_tool_use_id: None,
            tool_name: "read".to_string(),
            permission_source: None,
            source_thread_id: None,
            source_agent_label: None,
            kind: ToolPauseKind::Permission(PermissionPreview::Read(ReadPermissionPreview {
                file_path: file_path.to_string(),
            })),
        };
        state.apply_event(RuntimeToUiEvent::ToolPauseRequested(request(
            "read-1",
            "first.txt",
        )));
        state.apply_event(RuntimeToUiEvent::ToolPauseRequested(request(
            "read-2",
            "second.txt",
        )));

        terminal
            .draw(|frame| crate::app::draw(&mut state, frame))
            .unwrap();
        let timeline_before = state.sessions.views["main"]
            .selectable_message_lines
            .clone();
        let first_drawer = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(
            timeline_before
                .iter()
                .any(|line| line.contains("Read 2 files"))
        );
        assert!(first_drawer.contains("first.txt"));
        assert!(!first_drawer.contains("second.txt"));

        let removed_active = state.remove_tool_pause("read-1");
        state.finish_tool_pause_removal(removed_active);
        terminal
            .draw(|frame| crate::app::draw(&mut state, frame))
            .unwrap();

        let second_drawer = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert_eq!(
            state.sessions.views["main"].selectable_message_lines,
            timeline_before
        );
        assert!(second_drawer.contains("second.txt"));
        assert!(!second_drawer.contains("first.txt"));
    }

    #[test]
    fn permission_drawer_layout_leaves_gap_above_divider() {
        let backend = TestBackend::new(80, 18);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new();
        let mut input = std::collections::HashMap::new();
        input.insert("file_path".to_string(), serde_json::json!("Cargo.toml"));
        state.sessions.views["main"].pending_assistant = Some(
            Message::new(
                Role::Assistant,
                vec![ContentBlock::ToolUse(ToolUseBlock {
                    id: "read-1".to_string(),
                    name: "read".to_string(),
                    input,
                })],
            )
            .into(),
        );
        state.apply_event(RuntimeToUiEvent::ToolPauseRequested(ToolPauseRequest {
            tool_use_id: "read-1".to_string(),
            preview_tool_use_id: None,
            tool_name: "read".to_string(),
            permission_source: None,
            source_thread_id: None,
            source_agent_label: None,
            kind: ToolPauseKind::Permission(PermissionPreview::Read(ReadPermissionPreview {
                file_path: "Cargo.toml".to_string(),
            })),
        }));

        terminal
            .draw(|frame| crate::app::draw(&mut state, frame))
            .unwrap();

        let messages_bottom = state.geometry.messages_area.y + state.geometry.messages_area.height;
        let divider_top = state.geometry.permission_drawer_area.y.saturating_sub(1);
        assert_eq!(messages_bottom + 1, divider_top);
    }

    #[test]
    fn user_input_drawer_layout_leaves_gap_above_divider() {
        let backend = TestBackend::new(80, 18);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new();
        state.apply_event(RuntimeToUiEvent::ToolPauseRequested(ToolPauseRequest {
            tool_use_id: "ask-1".to_string(),
            preview_tool_use_id: None,
            tool_name: "ask_user".to_string(),
            permission_source: None,
            source_thread_id: None,
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

        terminal
            .draw(|frame| crate::app::draw(&mut state, frame))
            .unwrap();

        let messages_bottom = state.geometry.messages_area.y + state.geometry.messages_area.height;
        let divider_top = state.geometry.permission_drawer_area.y.saturating_sub(1);
        assert_eq!(messages_bottom + 1, divider_top);
    }

    #[test]
    fn plan_approval_drawer_renders_in_tiny_terminal() {
        let backend = TestBackend::new(80, 4);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new();
        state.dialogs.plan.plan_approval = Some(ProposedPlan {
            id: "20260522T000000Z-plan".to_string(),
            title: "Plan".to_string(),
            markdown: "# Plan\n\n- Step".to_string(),
            path: "/tmp/plan.md".into(),
            created_at: Utc::now(),
        });

        terminal
            .draw(|frame| crate::app::draw(&mut state, frame))
            .unwrap();
    }

    #[test]
    fn plan_approval_layout_leaves_gap_above_drawer() {
        let backend = TestBackend::new(80, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = AppState::new();
        state.dialogs.plan.plan_approval = Some(ProposedPlan {
            id: "20260522T000000Z-plan".to_string(),
            title: "Plan".to_string(),
            markdown: "# Plan\n\n- Step".to_string(),
            path: "/tmp/plan.md".into(),
            created_at: Utc::now(),
        });

        terminal
            .draw(|frame| crate::app::draw(&mut state, frame))
            .unwrap();

        let drawer_height =
            plan_approval_drawer::plan_approval_drawer_height(Rect::new(0, 0, 80, 12));
        let drawer_top = 12 - drawer_height;
        let messages_bottom = state.geometry.messages_area.y + state.geometry.messages_area.height;

        assert_eq!(messages_bottom + 1, drawer_top);
    }
}
