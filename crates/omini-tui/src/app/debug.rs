//! 离线 TUI 样式图鉴，调用生产环境的组件与布局，无需 daemon。
use crate::app::event::*;
use crate::app::state::{AppState, SessionState, SubagentNode, UiMessage};
use crate::features::agents::state::{AgentManagerState, InteractionStep, ModelSelectionEntry};
use crate::features::tools::render_tool;
use crate::platform::terminal;
use crate::ui::theme;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use jiff::Timestamp;
use omini_model::message::{ContentBlock, ToolResultBlock, ToolUseBlock};
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph},
};
use std::{collections::HashMap, io};

pub const TOOL_NAMES: &[&str] = &[
    "bash",
    "read",
    "search",
    "view_image",
    "skill",
    "edit",
    "write",
    "todo_write",
    "ask_user",
    "spawn_agent",
    "run_agent",
    "send_message",
    "mcp__docs__search",
    "custom_tool",
];
pub const SCREEN_NAMES: &[&str] = &[
    "输入框",
    "起始页",
    "Bash 审批",
    "文件审批",
    "MCP 审批",
    "通用工具审批",
    "Ask",
    "计划审批",
    "帮助",
    "会话列表",
    "模型选择",
    "Agent 管理",
    "首次配置",
    "Write 审批",
    "Read 审批",
    "Search 审批",
    "Ask 第二题",
    "计划 Auto",
    "主会话运行中",
    "子会话运行中",
    "子会话终态",
    "空闲且有任务",
    "空输入框",
];
const STATE_NAMES: &[&str] = &["仅调用", "返回结果", "失败结果", "取消结果", "中断结果"];

#[derive(Debug, Clone, Copy, Default)]
pub struct Gallery {
    pub scene: usize,
    pub scroll: usize,
}
impl Gallery {
    pub fn count() -> usize {
        TOOL_NAMES.len() + SCREEN_NAMES.len()
    }
    pub fn title(self) -> &'static str {
        if self.scene < TOOL_NAMES.len() {
            TOOL_NAMES[self.scene]
        } else {
            SCREEN_NAMES[self.scene - TOOL_NAMES.len()]
        }
    }
    pub fn key(&mut self, key: KeyCode) -> bool {
        match key {
            KeyCode::Char('q') | KeyCode::Esc => return false,
            KeyCode::Right | KeyCode::Tab | KeyCode::Char('l') => {
                self.scene = (self.scene + 1) % Self::count();
                self.scroll = 0;
            }
            KeyCode::Left | KeyCode::BackTab | KeyCode::Char('h') => {
                self.scene = (self.scene + Self::count() - 1) % Self::count();
                self.scroll = 0;
            }
            KeyCode::Down | KeyCode::Char('j') => self.scroll = self.scroll.saturating_add(1),
            KeyCode::Up | KeyCode::Char('k') => self.scroll = self.scroll.saturating_sub(1),
            KeyCode::PageDown => self.scroll = self.scroll.saturating_add(10),
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(10),
            KeyCode::Home => {
                self.scene = 0;
                self.scroll = 0;
            }
            KeyCode::End => {
                self.scene = Self::count() - 1;
                self.scroll = 0;
            }
            _ => {}
        }
        true
    }
}

pub fn run() -> io::Result<()> {
    let _guard = terminal::RestoreGuard::new();
    let mut terminal = terminal::init()?;
    let mut gallery = Gallery::default();
    terminal.draw(|frame| render(frame, gallery))?;
    loop {
        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                if !gallery.key(key.code) {
                    break;
                }
                terminal.draw(|frame| render(frame, gallery))?;
            }
            Event::Resize(_, _) => {
                terminal.draw(|frame| render(frame, gallery))?;
            }
            _ => {}
        }
    }
    terminal::restore(&mut terminal)
}

pub fn render(frame: &mut ratatui::Frame, gallery: Gallery) {
    let area = frame.area();
    if area.width == 0 || area.height == 0 {
        return;
    }
    frame.render_widget(Clear, area);
    frame.render_widget(
        Block::default().style(Style::default().fg(theme::TEXT).bg(theme::BACKGROUND)),
        area,
    );
    let screen = gallery.scene.saturating_sub(TOOL_NAMES.len());
    if gallery.scene >= TOOL_NAMES.len() && SCREEN_NAMES[screen] != "首次配置" {
        let mut state = sample_screen(screen);
        crate::app::draw(&mut state, frame);
    } else if gallery.scene >= TOOL_NAMES.len() {
        let form = crate::features::setup::state::ConfigurationForm::new(
            &omini_protocol::ProjectConfigurationResponse {
                state: omini_protocol::ProjectConfigurationState::SetupRequired,
                code: None,
                message: None,
                provider_id: Some("openai".into()),
            },
        );
        crate::features::setup::view::render_form(frame, &form);
    } else {
        render_tool_gallery(frame, gallery, area);
    }
    let title = format!(
        " TUI DEBUG · {}/{} · {} ",
        gallery.scene + 1,
        Gallery::count(),
        gallery.title()
    );
    let head = Rect::new(area.x, area.y, area.width, 1);
    theme::clear_panel(frame, head);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            title,
            Style::default()
                .fg(theme::ACCENT)
                .bg(theme::PANEL)
                .add_modifier(Modifier::BOLD),
        ))),
        head,
    );
    if area.height > 1 {
        let foot = Rect::new(area.x, area.bottom() - 1, area.width, 1);
        theme::clear_panel(frame, foot);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                " ←/→ 切换样式   ↑/↓ 滚动   Home/End 首末   q 退出 ",
                Style::default().fg(theme::MUTED).bg(theme::PANEL),
            ))),
            foot,
        );
    }
}

fn render_tool_gallery(frame: &mut ratatui::Frame, gallery: Gallery, area: Rect) {
    let tool = tool_sample(TOOL_NAMES[gallery.scene]);
    let mut lines = Vec::new();
    for (index, state) in STATE_NAMES.iter().enumerate() {
        lines.push(Line::from(Span::styled(
            format!("── {state} ──"),
            Style::default().fg(theme::BORDER),
        )));
        let result = tool_result(&tool, index);
        lines.extend(render_tool(
            &tool,
            result.as_ref(),
            None,
            None,
            area.width as usize,
            None,
        ));
        lines.push(Line::from(""));
    }
    let content = Rect::new(
        area.x,
        area.y.saturating_add(1),
        area.width,
        area.height.saturating_sub(2),
    );
    if content.height > 0 {
        frame.render_widget(
            Paragraph::new(
                lines
                    .into_iter()
                    .skip(gallery.scroll)
                    .take(content.height as usize)
                    .collect::<Vec<_>>(),
            )
            .style(Style::default().fg(theme::TEXT).bg(theme::BACKGROUND)),
            content,
        );
    }
}

fn tool_sample(name: &str) -> ToolUseBlock {
    let input = match name {
        "bash" => {
            serde_json::json!({"command":"cargo test -p omini-tui && echo '中文 ✓'", "description":"运行测试"})
        }
        "read" => serde_json::json!({"file_path":"src/main.rs", "offset":20,"limit":40}),
        "search" => serde_json::json!({"query":"ToolUse", "path":"src"}),
        "view_image" => serde_json::json!({"path":"assets/preview.png"}),
        "skill" => serde_json::json!({"name":"commit-message"}),
        "edit" => {
            serde_json::json!({"file_path":"src/main.rs", "old_string":"old", "new_string":"new"})
        }
        "write" => serde_json::json!({"file_path":"src/new.rs", "content":"pub fn hello() {}"}),
        "todo_write" => {
            serde_json::json!({"todos":[{"content":"读取旧实现","status":"completed"},{"content":"设计新组件","status":"in_progress"},{"content":"验证布局","status":"pending"},{"content":"过时事项","status":"cancelled"}]})
        }
        "ask_user" => {
            serde_json::json!({"questions":[{"id":"theme","header":"主题","question":"选择偏好的主题？","options":[{"label":"琥珀","description":"温暖"}]}]})
        }
        "spawn_agent" | "run_agent" => {
            serde_json::json!({"name":"Explore", "title":"梳理架构与历史事件"})
        }
        "send_message" => {
            serde_json::json!({"target":"task-1", "message":"请复查边界情况，并汇报剩余风险"})
        }
        "mcp__docs__search" => serde_json::json!({"query":"Ratatui", "limit":10}),
        _ => serde_json::json!({"question":"这是什么工具？", "count":2}),
    };
    ToolUseBlock {
        id: format!("debug-{name}"),
        name: name.to_owned(),
        input: input
            .as_object()
            .unwrap()
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    }
}

fn tool_result(tool: &ToolUseBlock, state: usize) -> Option<ToolResultBlock> {
    if state == 0 {
        return None;
    }
    let metadata = match tool.name.as_str() {
        "edit" | "write" => {
            serde_json::json!({"diff":"@@ -1,2 +1,2 @@\n-old\n+new\n context", "existed":false})
        }
        "search" => {
            serde_json::json!({"total":28,"shown":10,"files_with_matches":4,"truncated":true})
        }
        "view_image" => serde_json::json!({"width":1280,"height":720,"format":"PNG"}),
        _ => serde_json::json!({}),
    };
    let mut metadata = metadata.as_object().unwrap().clone();
    if state == 3 {
        metadata.insert("status".into(), serde_json::json!("cancelled"));
    }
    if state == 4 {
        metadata.insert("status".into(), serde_json::json!("interrupted"));
    }
    Some(ToolResultBlock {
        tool_use_id: tool.id.clone(),
        is_error: state == 2,
        content: match state {
            2 => "示例执行失败",
            3 => "执行已取消",
            4 => "执行已中断",
            _ => "第 1 行\n第 2 行\n示例结果",
        }
        .into(),
        metadata: Some(metadata),
    })
}

fn sample_screen(screen: usize) -> AppState {
    let mut state = AppState::new();
    state.composer.input_placeholder = "用 /help 查看命令、技能和输入技巧".into();
    state.start.show_start_screen = screen == 1;
    state.project.status_bar.cwd = std::path::PathBuf::from("/project");
    state.project.status_bar.model = "sample-model".into();
    state.project.status_bar.active_provider = "Example".into();
    if matches!(screen, 2..=11 | 13..=17) {
        state.sessions.views["main"]
            .messages
            .push(UiMessage::SystemEvent(
                crate::features::timeline::model::UiSystemEvent::UserInputEcho(
                    crate::features::timeline::model::UserDraft::plain(
                        "检查 TUI 的交互与工具展示".into(),
                    ),
                ),
            ));
        state.sessions.views["main"]
            .messages
            .extend(UiMessage::from_blocks(vec![ContentBlock::Text(
                omini_model::message::TextBlock {
                    text: "我会保留这里的会话上下文，供你对照抽屉。".into(),
                },
            )]));
    }
    match screen {
        0 => {
            state.composer.input = "请检查 @src/main.rs\n并解释中文宽度".into();
            state.composer.cursor_char = state.composer.input.chars().count();
        }
        1 => {}
        2..=5 | 13..=15 => {
            let name = match screen {
                2 => "bash",
                3 => "edit",
                4 => "mcp__docs__search",
                13 => "write",
                14 => "read",
                15 => "search",
                _ => "custom_tool",
            };
            let tool = tool_sample(name);
            state.sessions.views["main"].pending_assistant =
                Some(crate::features::timeline::model::StreamingMessage {
                    content: vec![ContentBlock::ToolUse(tool.clone())],
                });
            let kind = match screen {
                2 => ToolPauseKind::Permission(PermissionPreview::Bash(BashPermissionPreview {
                    command: tool.input["command"].as_str().unwrap().into(),
                    description: Some("运行测试".into()),
                    workdir: None,
                    timeout: 120_000,
                })),
                3 => ToolPauseKind::Permission(PermissionPreview::Edit(EditPermissionPreview {
                    summary: "Update src/main.rs".into(),
                    path: "src/main.rs".into(),
                    replacement_count: 1,
                    diff: "@@ -1,2 +1,2 @@\n-old\n+new\n context".into(),
                })),
                13 => ToolPauseKind::Permission(PermissionPreview::Write(EditPermissionPreview {
                    summary: "Create src/new.rs".into(),
                    path: "src/new.rs".into(),
                    replacement_count: 1,
                    diff: "@@ -0,0 +1 @@\n+pub fn hello() {}".into(),
                })),
                14 => ToolPauseKind::Permission(PermissionPreview::Read(ReadPermissionPreview {
                    file_path: "/project/src/main.rs".into(),
                })),
                15 => {
                    ToolPauseKind::Permission(PermissionPreview::Search(SearchPermissionPreview {
                        query: "ToolUse".into(),
                        mode: "content".into(),
                        path: "/project/src".into(),
                    }))
                }
                4 => ToolPauseKind::Permission(PermissionPreview::Mcp(McpPermissionPreview {
                    server_name: "docs".into(),
                    server_tool_name: "search".into(),
                    registered_tool_name: name.into(),
                    inputs: tool.input.clone().into_iter().collect(),
                })),
                _ => ToolPauseKind::Permission(PermissionPreview::Custom {
                    tool_name: name.into(),
                    payload: tool.input.clone().into_iter().collect(),
                }),
            };
            state.apply_event(RuntimeToUiEvent::ToolPauseRequested(ToolPauseRequest {
                tool_use_id: tool.id,
                preview_tool_use_id: None,
                tool_name: name.into(),
                permission_source: None,
                source_thread_id: None,
                source_agent_label: None,
                kind,
            }));
        }
        6 | 16 => {
            state.apply_event(RuntimeToUiEvent::ToolPauseRequested(ToolPauseRequest {
                tool_use_id: "ask".into(),
                preview_tool_use_id: None,
                tool_name: "ask_user".into(),
                permission_source: None,
                source_thread_id: None,
                source_agent_label: None,
                kind: ToolPauseKind::UserInput(UserInputPreview {
                    questions: vec![
                        UserInputQuestion {
                            id: "color".into(),
                            header: "配色".into(),
                            question: "你更喜欢哪一种主色？".into(),
                            options: vec![
                                UserInputOption {
                                    label: "暖琥珀".into(),
                                    description: "温和、清晰".into(),
                                },
                                UserInputOption {
                                    label: "雾蓝".into(),
                                    description: "冷静、低饱和".into(),
                                },
                            ],
                        },
                        UserInputQuestion {
                            id: "layout".into(),
                            header: "布局".into(),
                            question: "选择输入框样式".into(),
                            options: vec![UserInputOption {
                                label: "上下细线".into(),
                                description: "与参考图一致".into(),
                            }],
                        },
                    ],
                }),
            }));
            if screen == 16 {
                state.dialogs.ask.user_input_question_index = 1;
                state.dialogs.ask.user_input_answered[0] = true;
                state.dialogs.ask.user_input_note_mode = true;
                state.dialogs.ask.user_input_notes[1] = "保留三行正文".into();
            }
        }
        7 | 17 => {
            state.open_plan_approval(SubmittedPlan {
                id: "debug-plan".into(),
                title: "优化 TUI".into(),
                markdown: "# 计划\n\n重新设计主题、输入框与工具展示。".into(),
                path: "/tmp/debug-plan.md".into(),
                created_at: Timestamp::now(),
            });
            if screen == 17 {
                state.dialogs.plan.plan_approval_auto = true;
                state.dialogs.plan.plan_approval_selected = 1;
            }
        }
        8 => {
            state.dialogs.help_drawer = Some(crate::features::help::state::HelpDrawerState::new(
                Vec::new(),
            ))
        }
        9 => {
            let now = Timestamp::now();
            let thread = ThreadSummary {
                id: "debug".into(),
                title: "TUI 样式检查".into(),
                model: "sample-model".into(),
                provider: "Example".into(),
                created_at: now,
                updated_at: now,
                runtime_state: None,
            };
            state.dialogs.interaction_step = Some(InteractionStep::Thread {
                threads: vec![thread.clone()],
                all_threads: vec![thread],
                search: String::new(),
                selected: 0,
            });
        }
        10 => {
            state.dialogs.interaction_step = Some(InteractionStep::ModelSelection {
                entries: vec![
                    ModelSelectionEntry::ProviderHeader {
                        name: "Example".into(),
                    },
                    ModelSelectionEntry::Model {
                        provider_key: "Example".into(),
                        model: crate::client::catalog::ModelConfig {
                            id: "sample-model".into(),
                            name: Some("Sample Model".into()),
                            limit: 128_000,
                            thinking: true,
                            input_modalities: None,
                            extra_headers: None,
                            extra_body: None,
                        },
                    },
                ],
                selected: 1,
                thinking_idx: 2,
                active_provider: "Example".into(),
                active_model: "sample-model".into(),
            })
        }
        11 => {
            let record = omini_domain::subagents::AgentRecord {
                name: "Explore".into(),
                description: "梳理代码结构与历史事件".into(),
                short_description: None,
                instructions: "阅读并报告".into(),
                tools: vec!["read".into(), "search".into()],
                disallow_tools: Vec::new(),
                model: None,
                source_kind: omini_domain::subagents::AgentSourceKind::Project,
                path: None,
                editable: true,
            };
            state.dialogs.interaction_step =
                Some(InteractionStep::Agents(Box::new(AgentManagerState::new(
                    vec![record],
                    HashMap::new(),
                    "Example".into(),
                    "sample-model".into(),
                ))));
        }
        18 => {
            state.sessions.views["main"].agent_status =
                crate::features::sessions::model::AgentStatus::Thinking;
            state.sessions.views["main"].pending_assistant =
                Some(crate::features::timeline::model::StreamingMessage {
                    content: vec![ContentBlock::Text(omini_model::message::TextBlock {
                        text: "正在分析项目结构与依赖……".into(),
                    })],
                });
        }
        19 | 20 => {
            let completed = screen == 20;
            let task_id = "debug-child".to_string();
            state.sessions.subagent_order.push(task_id.clone());
            state.sessions.subagents.insert(
                "debug-thread".into(),
                SubagentNode {
                    task_id: task_id.clone(),
                    thread_id: "debug-thread".into(),
                    parent_thread_id: "main".into(),
                    spawn_tool_use_id: "debug-spawn".into(),
                    agent_label: "Explore".into(),
                    title: "检查工具视图".into(),
                    execution_mode: AgentTaskExecutionMode::Background,
                    status: if completed {
                        omini_domain::task::TaskStatus::Completed
                    } else {
                        omini_domain::task::TaskStatus::Running
                    },
                    duration: completed.then_some(std::time::Duration::from_secs(12)),
                    started_at: Timestamp::now(),
                    messages: Vec::new(),
                },
            );
            state.sessions.views.insert(
                task_id.clone(),
                SessionState {
                    messages: vec![UiMessage::SystemEvent(
                        crate::features::timeline::model::UiSystemEvent::UserInputEcho(
                            crate::features::timeline::model::UserDraft::plain(
                                "检查工具视图".into(),
                            ),
                        ),
                    )],
                    agent_status: if completed {
                        crate::features::sessions::model::AgentStatus::Idle
                    } else {
                        crate::features::sessions::model::AgentStatus::Working
                    },
                    ..SessionState::default()
                },
            );
            state.sessions.active_session_task_id = Some(task_id);
        }
        21 => {
            state.track_background_task(
                "debug-child".into(),
                omini_domain::task::TaskStatus::Running,
            );
        }
        _ => {}
    }
    state
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;
    use ratatui::{Terminal, backend::TestBackend};
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn idle_task_scene_shows_wait_hint() {
        let scene = TOOL_NAMES.len() + 21;
        for (width, height) in [(120, 36), (80, 24)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| render(frame, Gallery { scene, scroll: 0 }))
                .unwrap();
            let content = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(content.contains("Waiting for 1 background task to finish"));
        }
    }

    #[test]
    fn all_styles_render_at_both_sizes() {
        for (width, height) in [(120, 36), (80, 24)] {
            for scene in 0..Gallery::count() {
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                terminal
                    .draw(|frame| render(frame, Gallery { scene, scroll: 0 }))
                    .unwrap();
                let content = terminal
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect::<String>();
                assert!(content.contains("TUI DEBUG"), "{scene}");
                let name = if scene < TOOL_NAMES.len() {
                    TOOL_NAMES[scene]
                } else {
                    SCREEN_NAMES[scene - TOOL_NAMES.len()]
                };
                assert!(
                    content.replace(' ', "").contains(&name.replace(' ', "")),
                    "{scene}"
                );
                let buffer = terminal.backend().buffer();
                let default_background = buffer.content().iter().enumerate().any(|(i, cell)| {
                    cell.bg == Color::Reset
                        && !(i % width as usize > 0
                            && UnicodeWidthStr::width(buffer.content()[i - 1].symbol()) > 1)
                });
                assert!(default_background, "样式 {scene} 没有终端默认背景区域");
                assert_eq!(buffer[(0, 0)].bg, theme::PANEL);
            }
        }
    }

    #[test]
    fn empty_input_has_reference_lines_and_dim_suggestion() {
        let scene = TOOL_NAMES.len() + SCREEN_NAMES.len() - 1;
        for (width, height) in [(120, 36), (80, 24)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| render(frame, Gallery { scene, scroll: 0 }))
                .unwrap();
            let buffer = terminal.backend().buffer();
            let y = (1..height - 1)
                .find(|y| buffer[(0, *y)].symbol() == "❯")
                .unwrap();
            assert_eq!(buffer[(0, y - 1)].symbol(), "─");
            assert_eq!(buffer[(0, y + 1)].symbol(), "─");
            assert_eq!(buffer[(2, y)].symbol(), " ");
            assert_eq!(buffer[(3, y)].symbol(), "用");
            assert_eq!(buffer[(3, y)].fg, theme::MUTED);
            assert!(buffer[(3, y)].modifier.contains(Modifier::DIM));
        }
    }

    #[test]
    fn selection_views_keep_conversation_visible() {
        for (width, height) in [(120, 36), (80, 24)] {
            for screen in [10, 11] {
                let mut state = sample_screen(screen);
                state.sessions.views["main"]
                    .messages
                    .push(UiMessage::SystemEvent(
                        crate::features::timeline::model::UiSystemEvent::UserInputEcho(
                            crate::features::timeline::model::UserDraft::plain(
                                "history-stays-visible".into(),
                            ),
                        ),
                    ));
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                terminal
                    .draw(|frame| crate::app::draw(&mut state, frame))
                    .unwrap();
                let text = terminal
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect::<String>();
                assert!(
                    text.contains("history-stays-visible"),
                    "view {screen} at {width}×{height}"
                );
            }
        }
    }
}
