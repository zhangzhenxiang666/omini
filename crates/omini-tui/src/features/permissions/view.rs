use crate::ui::context::ViewContext;
use crate::ui::drawer::*;
use crate::ui::prelude::*;
pub fn build_permission_action_lines(
    state: &ViewContext<'_>,
    request: &ToolPauseRequest,
) -> Text<'static> {
    let yes_style = permission_option_style(state.dialogs.permission.permission_selected == 0);
    let no_style = permission_option_style(state.dialogs.permission.permission_selected == 1);
    let (yes_desc, no_desc) = permission_option_descriptions(request);
    let desc_style = Style::default().fg(crate::ui::theme::MUTED);
    let note_hint = if state.note_mode() {
        "Tab/Esc end note"
    } else {
        "Tab note"
    };
    Text::from(vec![
        Line::from(vec![
            Span::styled("1. ", yes_style),
            Span::styled(
                format!(
                    "{:<3}",
                    if request.tool_name == "bash" {
                        "运行"
                    } else {
                        "允许"
                    }
                ),
                yes_style,
            ),
            Span::raw("   "),
            Span::styled(yes_desc, desc_style),
        ]),
        Line::from(vec![
            Span::styled("2. ", no_style),
            Span::styled(format!("{:<3}", "跳过"), no_style),
            Span::raw("   "),
            Span::styled(format!("{no_desc} · {note_hint}"), desc_style),
        ]),
    ])
}

pub fn permission_option_descriptions(request: &ToolPauseRequest) -> (&'static str, &'static str) {
    match &request.kind {
        ToolPauseKind::Permission(PermissionPreview::Bash(_)) => ("run command", "skip command"),
        ToolPauseKind::Permission(PermissionPreview::Edit(_)) => {
            ("apply changes", "reject changes")
        }
        ToolPauseKind::Permission(PermissionPreview::Write(_)) => ("write file", "reject write"),
        ToolPauseKind::Permission(PermissionPreview::Read(_))
            if request.tool_name == "view_image" =>
        {
            ("view image", "skip view")
        }
        ToolPauseKind::Permission(PermissionPreview::Read(_)) => ("read file", "skip read"),
        ToolPauseKind::Permission(PermissionPreview::Search(_)) => ("search path", "skip search"),
        ToolPauseKind::Permission(PermissionPreview::Mcp(_)) => ("call tool", "deny tool"),
        ToolPauseKind::Permission(PermissionPreview::Custom { .. }) => ("allow tool", "deny tool"),
        ToolPauseKind::UserInput(_) => ("提交回答", "取消请求"),
    }
}

pub fn build_permission_drawer_lines(input: PermissionDrawerLinesInput<'_>) -> DrawerLines {
    let PermissionDrawerLinesInput {
        request,
        tool_use,
        content_width,
        project_dir,
        question_index: _,
        user_input_selected: _,
        current_user_input_note,
        user_input_note_cursor,
        user_input_note_mode,
    } = input;

    let mut drawer = match &request.kind {
        ToolPauseKind::Permission(PermissionPreview::Bash(preview)) => {
            let mut lines = Vec::new();
            lines.push(Line::from(Span::styled(
                "$ Shell",
                Style::default().fg(crate::ui::theme::ACCENT),
            )));

            if let Some(description) = &preview.description
                && !description.trim().is_empty()
            {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled("说明：", Style::default().fg(crate::ui::theme::MUTED)),
                    Span::styled(
                        description.trim().to_string(),
                        Style::default().fg(crate::ui::theme::TEXT),
                    ),
                ]));
                lines.push(Line::from(""));
            }
            lines.extend(bash_permission_command_lines(
                &preview.command,
                content_width,
            ));
            DrawerLines {
                lines,
                note_lines: Vec::new(),
                note_cursor: None,
            }
        }
        ToolPauseKind::Permission(PermissionPreview::Edit(_)) => {
            let lines = if let Some(tool_use) = tool_use {
                let placeholder = crate::features::tools::preview_placeholder_result(tool_use);
                render_tool(
                    tool_use,
                    Some(&placeholder),
                    Some(request),
                    None,
                    content_width,
                    project_dir,
                )
            } else {
                vec![Line::from(Span::styled(
                    "缺少编辑预览所需的工具输入",
                    Style::default().fg(crate::ui::theme::ERROR),
                ))]
            };
            DrawerLines {
                lines,
                note_lines: Vec::new(),
                note_cursor: None,
            }
        }
        ToolPauseKind::Permission(PermissionPreview::Write(_)) => {
            let lines = if let Some(tool_use) = tool_use {
                let placeholder = crate::features::tools::preview_placeholder_result(tool_use);
                render_tool(
                    tool_use,
                    Some(&placeholder),
                    Some(request),
                    None,
                    content_width,
                    project_dir,
                )
            } else {
                vec![Line::from(Span::styled(
                    "缺少写入预览所需的工具输入",
                    Style::default().fg(crate::ui::theme::ERROR),
                ))]
            };
            DrawerLines {
                lines,
                note_lines: Vec::new(),
                note_cursor: None,
            }
        }
        ToolPauseKind::Permission(PermissionPreview::Read(preview)) => {
            let lines = if let Some(tool_use) = tool_use {
                let placeholder = crate::features::tools::preview_placeholder_result(tool_use);
                render_tool(
                    tool_use,
                    Some(&placeholder),
                    Some(request),
                    None,
                    content_width,
                    project_dir,
                )
            } else {
                vec![Line::from(vec![
                    Span::raw("  "),
                    Span::styled("路径：", Style::default().fg(crate::ui::theme::MUTED)),
                    Span::styled(
                        display_path(&preview.file_path, project_dir),
                        Style::default().fg(crate::ui::theme::TEXT),
                    ),
                ])]
            };
            DrawerLines {
                lines,
                note_lines: Vec::new(),
                note_cursor: None,
            }
        }
        ToolPauseKind::Permission(PermissionPreview::Search(preview)) => {
            let mode = match preview.mode.as_str() {
                "files" => "文件名",
                _ => "内容",
            };
            let mut lines = vec![Line::from("")];
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled("模式：", Style::default().fg(crate::ui::theme::MUTED)),
                Span::styled(
                    mode.to_string(),
                    Style::default().fg(crate::ui::theme::TEXT),
                ),
            ]));
            if !preview.query.trim().is_empty() {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled("查询：", Style::default().fg(crate::ui::theme::MUTED)),
                    Span::styled(
                        preview.query.trim().to_string(),
                        Style::default().fg(crate::ui::theme::TEXT),
                    ),
                ]));
            }
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled("路径：", Style::default().fg(crate::ui::theme::MUTED)),
                Span::styled(
                    display_path(&preview.path, project_dir),
                    Style::default().fg(crate::ui::theme::TEXT),
                ),
            ]));
            DrawerLines {
                lines,
                note_lines: Vec::new(),
                note_cursor: None,
            }
        }
        ToolPauseKind::Permission(PermissionPreview::Mcp(preview)) => DrawerLines {
            lines: mcp_permission_lines(preview, content_width),
            note_lines: Vec::new(),
            note_cursor: None,
        },
        ToolPauseKind::Permission(PermissionPreview::Custom { tool_name, payload }) => {
            DrawerLines {
                lines: std::iter::once(Line::from(Span::styled(
                    tool_name.clone(),
                    Style::default().fg(crate::ui::theme::ACCENT),
                )))
                .chain(
                    crate::features::tools::word_wrap(
                        &serde_json::to_string_pretty(payload).unwrap_or_default(),
                        content_width.max(1),
                    )
                    .into_iter()
                    .map(Line::from),
                )
                .collect(),
                note_lines: Vec::new(),
                note_cursor: None,
            }
        }
        ToolPauseKind::UserInput(preview) => {
            crate::features::questions::view::build_question_drawer(input, preview)
        }
    };
    if matches!(&request.kind, ToolPauseKind::Permission(_)) {
        set_note_line(
            &mut drawer,
            current_user_input_note,
            user_input_note_cursor,
            user_input_note_mode,
            true,
            content_width,
        );
    }
    add_permission_source_line(&mut drawer, request);
    drawer
}

pub fn mcp_permission_lines(
    preview: &crate::app::event::McpPermissionPreview,
    content_width: usize,
) -> Vec<Line<'static>> {
    let label_style = Style::default().fg(crate::ui::theme::MUTED);
    let text_style = Style::default()
        .fg(crate::ui::theme::TEXT)
        .add_modifier(Modifier::BOLD);
    let json_style = Style::default().fg(crate::ui::theme::MUTED);
    let mut lines = vec![Line::from("")];

    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(preview.server_name.clone(), text_style),
        Span::styled(" / ", Style::default().fg(crate::ui::theme::BORDER)),
        Span::styled(preview.server_tool_name.clone(), text_style),
    ]));

    let json = serde_json::to_string_pretty(&serde_json::Value::Object(preview.inputs.clone()))
        .unwrap_or_else(|_| "{}".to_string());
    if json == "{}" {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled("输入 ", label_style),
            Span::styled("{}", json_style),
        ]));
        return lines;
    }

    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("输入", label_style),
    ]));

    let json_width = content_width.saturating_sub(2).max(1);
    for line in json.lines() {
        let wrapped = wrap_preserving_display_width(line, json_width);
        for segment in wrapped {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(segment, json_style),
            ]));
        }
    }
    lines
}

pub fn add_permission_source_line(drawer: &mut DrawerLines, request: &ToolPauseRequest) {
    if drawer.lines.is_empty() {
        return;
    };

    let mut insert_at = 1usize;
    if let Some(label) = request.source_agent_label.as_deref() {
        drawer.lines.insert(
            insert_at,
            Line::from(vec![
                Span::raw("  "),
                Span::styled("来源：", Style::default().fg(crate::ui::theme::MUTED)),
                Span::styled(
                    label.to_string(),
                    Style::default().fg(crate::ui::theme::TEXT),
                ),
            ]),
        );
        insert_at += 1;
    }

    if let Some(source) = &request.permission_source {
        drawer.lines.insert(
            insert_at,
            Line::from(vec![
                Span::raw("  "),
                Span::styled("规则：", Style::default().fg(crate::ui::theme::MUTED)),
                Span::styled(
                    source.decision.clone(),
                    Style::default().fg(crate::ui::theme::TEXT),
                ),
            ]),
        );
        drawer.lines.insert(
            insert_at + 1,
            Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    source.source.clone(),
                    Style::default().fg(crate::ui::theme::MUTED),
                ),
                Span::styled(" -> ", Style::default().fg(crate::ui::theme::BORDER)),
                Span::styled(
                    source.rule.clone(),
                    Style::default().fg(crate::ui::theme::MUTED),
                ),
            ]),
        );
        insert_at += 2;
    }

    if insert_at == 1 {
        return;
    }
    drawer.lines.insert(insert_at, Line::from(""));
}

pub fn bash_permission_command_lines(command: &str, content_width: usize) -> Vec<Line<'static>> {
    let prompt_style = Style::default().fg(crate::features::tools::bash_highlight::PROMPT_FG);
    let command_style =
        Style::default().fg(crate::features::tools::bash_highlight::COMMAND_TEXT_FG);
    let prefix = "  ";
    let prompt = "$ ";
    let continuation = "    ";
    let command_width = content_width
        .saturating_sub(prefix.width())
        .saturating_sub(prompt.width())
        .max(1);
    let wrapped = crate::features::tools::bash_highlight::wrapped_command_spans(
        command,
        command_width,
        command_style,
    );

    if wrapped.is_empty() {
        return vec![Line::from(vec![
            Span::raw(prefix),
            Span::styled(prompt, prompt_style),
        ])];
    }

    wrapped
        .into_iter()
        .enumerate()
        .map(|(idx, mut segment)| {
            if idx == 0 {
                let mut spans = vec![Span::raw(prefix), Span::styled(prompt, prompt_style)];
                spans.append(&mut segment);
                Line::from(spans)
            } else {
                let mut spans = vec![Span::raw(continuation)];
                spans.append(&mut segment);
                Line::from(spans)
            }
        })
        .collect()
}

pub fn permission_drawer_title(request: &ToolPauseRequest) -> &'static str {
    match &request.kind {
        ToolPauseKind::Permission(PermissionPreview::Read(_))
            if request.tool_name == "view_image" =>
        {
            "View Image"
        }
        ToolPauseKind::Permission(preview) => permission_preview_title(preview),
        ToolPauseKind::UserInput(_) => "Question",
    }
}

pub fn permission_preview_title(preview: &PermissionPreview) -> &'static str {
    match preview {
        PermissionPreview::Bash(_) => "Run Command",
        PermissionPreview::Edit(_) => "Edit File",
        PermissionPreview::Write(_) => "Write File",
        PermissionPreview::Read(_) => "Read File",
        PermissionPreview::Search(_) => "Search Files",
        PermissionPreview::Mcp(_) => "MCP Tool",
        PermissionPreview::Custom { .. } => "Tool Permission",
    }
}

pub fn find_tool_use(state: &ViewContext<'_>, tool_use_id: &str) -> Option<ToolUseBlock> {
    for session in state.sessions.views.values() {
        if let Some(pending) = &session.pending_assistant
            && let Some(tool) = pending.content.iter().find_map(|block| match block {
                ContentBlock::ToolUse(tool) if tool.id == tool_use_id => Some(tool.clone()),
                _ => None,
            })
        {
            return Some(tool);
        }
        for message in &session.messages {
            if let UiMessage::AssistantMessage(message) = message
                && let Some(tool) = message.blocks.iter().find_map(|block| match block {
                    omini_domain::conversation::AssistantMessageBlock::ToolUse {
                        id,
                        name,
                        input,
                    } if id == tool_use_id => Some(ToolUseBlock {
                        id: id.clone(),
                        name: name.clone(),
                        input: input.clone(),
                    }),
                    _ => None,
                })
            {
                return Some(tool);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::event::*;

    fn permission_request() -> ToolPauseRequest {
        ToolPauseRequest {
            tool_use_id: "tool_1".to_string(),
            preview_tool_use_id: None,
            tool_name: "write".to_string(),
            permission_source: None,
            source_thread_id: None,
            source_agent_label: None,
            kind: ToolPauseKind::Permission(PermissionPreview::Custom {
                tool_name: "write".to_string(),
                payload: serde_json::Map::new(),
            }),
        }
    }

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    fn has_exact_fg(line: &Line<'_>, text: &str, color: Color) -> bool {
        line.spans
            .iter()
            .any(|span| span.content.contains(text) && span.style.fg == Some(color))
    }

    #[test]
    fn view_image_read_permission_uses_view_image_copy() {
        let request = ToolPauseRequest {
            tool_use_id: "tool_1".to_string(),
            preview_tool_use_id: None,
            tool_name: "view_image".to_string(),
            permission_source: None,
            source_thread_id: None,
            source_agent_label: None,
            kind: ToolPauseKind::Permission(PermissionPreview::Read(
                crate::app::event::ReadPermissionPreview {
                    file_path: "/tmp/image.png".to_string(),
                },
            )),
        };

        assert_eq!(permission_drawer_title(&request), "View Image");
        assert_eq!(
            permission_option_descriptions(&request),
            ("view image", "skip view")
        );
    }

    #[test]
    fn mcp_permission_drawer_renders_service_tool_and_inputs() {
        let mut inputs = serde_json::Map::new();
        inputs.insert("query".to_string(), serde_json::json!("rust"));
        let request = ToolPauseRequest {
            tool_use_id: "tool_1".to_string(),
            preview_tool_use_id: None,
            tool_name: "mcp__docs__search".to_string(),
            permission_source: None,
            source_thread_id: None,
            source_agent_label: None,
            kind: ToolPauseKind::Permission(PermissionPreview::Mcp(
                crate::app::event::McpPermissionPreview {
                    server_name: "docs".to_string(),
                    server_tool_name: "search".to_string(),
                    registered_tool_name: "mcp__docs__search".to_string(),
                    inputs,
                },
            )),
        };

        let drawer = build_permission_drawer_lines(PermissionDrawerLinesInput {
            request: &request,
            tool_use: None,
            content_width: 80,
            project_dir: None,
            question_index: 0,
            user_input_selected: 0,
            current_user_input_note: "",
            user_input_note_cursor: 0,
            user_input_note_mode: false,
        });
        let lines = drawer.lines.iter().map(line_text).collect::<Vec<_>>();

        assert!(lines.iter().any(|line| line.contains("docs / search")));
        assert!(!lines.iter().any(|line| line.contains("mcp__docs__search")));
        assert!(
            lines
                .iter()
                .any(|line| line.contains("\"query\": \"rust\""))
        );
    }

    #[test]
    fn permission_note_is_fixed_outside_scroll_lines() {
        let request = permission_request();
        let drawer = build_permission_drawer_lines(PermissionDrawerLinesInput {
            request: &request,
            tool_use: None,
            content_width: 80,
            project_dir: None,
            question_index: 0,
            user_input_selected: 0,
            current_user_input_note: "Use English comments",
            user_input_note_cursor: 3,
            user_input_note_mode: true,
        });

        assert!(
            drawer
                .note_lines
                .iter()
                .any(|line| line_text(line).contains("Use English comments"))
        );
        assert!(
            !drawer
                .lines
                .iter()
                .any(|line| line_text(line).contains("Use English comments"))
        );
    }

    #[test]
    fn permission_note_line_is_reserved_blank_when_empty() {
        let request = permission_request();
        let drawer = build_permission_drawer_lines(PermissionDrawerLinesInput {
            request: &request,
            tool_use: None,
            content_width: 80,
            project_dir: None,
            question_index: 0,
            user_input_selected: 0,
            current_user_input_note: "",
            user_input_note_cursor: 0,
            user_input_note_mode: false,
        });

        assert_eq!(drawer.note_lines.len(), 1);
        assert_eq!(line_text(&drawer.note_lines[0]), "");
    }

    #[test]
    fn permission_note_wraps_and_moves_cursor_to_wrapped_line() {
        let request = permission_request();
        let note = "Use English comments and keep the approval reason concise";
        let drawer = build_permission_drawer_lines(PermissionDrawerLinesInput {
            request: &request,
            tool_use: None,
            content_width: 18,
            project_dir: None,
            question_index: 0,
            user_input_selected: 0,
            current_user_input_note: note,
            user_input_note_cursor: note.chars().count(),
            user_input_note_mode: true,
        });

        assert!(drawer.note_lines.len() > 1);
        let cursor = drawer.note_cursor.expect("note cursor should render");
        assert!(cursor.row > 0);
        assert!(
            !drawer
                .lines
                .iter()
                .any(|line| line_text(line).contains("approval reason"))
        );
    }

    #[test]
    fn user_input_note_wraps() {
        let request = ToolPauseRequest {
            tool_use_id: "ask-1".to_string(),
            preview_tool_use_id: None,
            tool_name: "ask_user".to_string(),
            permission_source: None,
            source_thread_id: None,
            source_agent_label: None,
            kind: ToolPauseKind::UserInput(crate::app::event::UserInputPreview {
                questions: vec![crate::app::event::UserInputQuestion {
                    id: "choice".to_string(),
                    header: "Choice".to_string(),
                    question: "Pick one".to_string(),
                    options: vec![crate::app::event::UserInputOption {
                        label: "First".to_string(),
                        description: "Use the first option".to_string(),
                    }],
                }],
            }),
        };
        let note = "A longer custom ask_user note should wrap cleanly";
        let drawer = build_permission_drawer_lines(PermissionDrawerLinesInput {
            request: &request,
            tool_use: None,
            content_width: 16,
            project_dir: None,
            question_index: 0,
            user_input_selected: 0,
            current_user_input_note: note,
            user_input_note_cursor: note.chars().count(),
            user_input_note_mode: true,
        });

        assert!(drawer.note_lines.len() > 1);
        assert!(drawer.note_cursor.is_some());
    }

    #[test]
    fn user_input_option_descriptions_align_with_wide_labels() {
        let request = ToolPauseRequest {
            tool_use_id: "ask-1".to_string(),
            preview_tool_use_id: None,
            tool_name: "ask_user".to_string(),
            permission_source: None,
            source_thread_id: None,
            source_agent_label: None,
            kind: ToolPauseKind::UserInput(crate::app::event::UserInputPreview {
                questions: vec![crate::app::event::UserInputQuestion {
                    id: "choice".to_string(),
                    header: "Choice".to_string(),
                    question: "Pick one".to_string(),
                    options: vec![
                        crate::app::event::UserInputOption {
                            label: "A".to_string(),
                            description: "Short desc".to_string(),
                        },
                        crate::app::event::UserInputOption {
                            label: "中文选项".to_string(),
                            description: "Wide desc".to_string(),
                        },
                    ],
                }],
            }),
        };
        let drawer = build_permission_drawer_lines(PermissionDrawerLinesInput {
            request: &request,
            tool_use: None,
            content_width: 80,
            project_dir: None,
            question_index: 0,
            user_input_selected: 0,
            current_user_input_note: "",
            user_input_note_cursor: 0,
            user_input_note_mode: false,
        });
        let rendered: Vec<String> = drawer.lines.iter().map(line_text).collect();
        let short = rendered
            .iter()
            .find(|line| line.contains("Short desc"))
            .expect("short option should render");
        let wide = rendered
            .iter()
            .find(|line| line.contains("Wide desc"))
            .expect("wide option should render");
        let short_prefix_width = short
            .split("Short desc")
            .next()
            .expect("short description prefix")
            .width();
        let wide_prefix_width = wide
            .split("Wide desc")
            .next()
            .expect("wide description prefix")
            .width();

        assert_eq!(short_prefix_width, wide_prefix_width);
    }

    #[test]
    fn permission_actions_remain_visible_in_note_mode() {
        let request = permission_request();
        let mut state = AppState::new();
        state.set_note_mode(true);
        state.dialogs.permission.permission_selected = 1;
        let actions = build_permission_action_lines(&ViewContext::new(&state), &request);
        let rendered: Vec<String> = actions.lines.iter().map(line_text).collect();

        assert!(rendered.iter().any(|line| line.contains("1. 允许")));
        assert!(rendered.iter().any(|line| line.contains("2. 跳过")));
        assert!(
            rendered
                .iter()
                .any(|line| line.contains("Tab/Esc end note"))
        );
    }

    #[test]
    fn search_permission_renders_query_and_path() {
        let relative_path = Path::new(".omini").join("skills").join("uumit-agent");
        let search_path = dirs::home_dir()
            .expect("home directory should be available")
            .join(&relative_path);
        let request = ToolPauseRequest {
            tool_use_id: "search-1".to_string(),
            preview_tool_use_id: None,
            tool_name: "search".to_string(),
            permission_source: None,
            source_thread_id: None,
            source_agent_label: None,
            kind: ToolPauseKind::Permission(PermissionPreview::Search(
                crate::app::event::SearchPermissionPreview {
                    query: "POST /api/v1/skills".to_string(),
                    mode: "content".to_string(),
                    path: search_path.display().to_string(),
                },
            )),
        };
        let drawer = build_permission_drawer_lines(PermissionDrawerLinesInput {
            request: &request,
            tool_use: None,
            content_width: 80,
            project_dir: None,
            question_index: 0,
            user_input_selected: 0,
            current_user_input_note: "",
            user_input_note_cursor: 0,
            user_input_note_mode: false,
        });
        let rendered = drawer.lines.iter().map(line_text).collect::<String>();

        assert!(rendered.contains("POST /api/v1/skills"));
        assert!(rendered.contains(&format!("~/{}", relative_path.display())));
    }

    #[test]
    fn bash_permission_wraps_long_command_without_truncating() {
        let command = "cargo test -p omini-tui permission_drawer_with_a_very_long_filter_name_that_exceeds_the_drawer_width -- --nocapture";
        let request = ToolPauseRequest {
            tool_use_id: "bash-1".to_string(),
            preview_tool_use_id: None,
            tool_name: "bash".to_string(),
            permission_source: None,
            source_thread_id: None,
            source_agent_label: None,
            kind: ToolPauseKind::Permission(PermissionPreview::Bash(
                crate::app::event::BashPermissionPreview {
                    command: command.to_string(),
                    description: None,
                    workdir: None,
                    timeout: 120_000,
                },
            )),
        };
        let drawer = build_permission_drawer_lines(PermissionDrawerLinesInput {
            request: &request,
            tool_use: None,
            content_width: 32,
            project_dir: None,
            question_index: 0,
            user_input_selected: 0,
            current_user_input_note: "",
            user_input_note_cursor: 0,
            user_input_note_mode: false,
        });
        let command_lines: Vec<String> = drawer
            .lines
            .iter()
            .map(line_text)
            .filter(|line| line.starts_with("  $ ") || line.starts_with("    "))
            .map(|line| {
                line.strip_prefix("  $ ")
                    .or_else(|| line.strip_prefix("    "))
                    .unwrap_or(&line)
                    .to_string()
            })
            .collect();

        assert!(command_lines.len() > 1);
        assert_eq!(command_lines.concat(), command);
    }

    #[test]
    fn bash_permission_uses_codex_command_palette() {
        let command = "cargo test -p omini-tui 'quoted value'";
        let lines = bash_permission_command_lines(command, 80);
        let first = &lines[0];

        assert_eq!(
            line_text(first),
            "  $ cargo test -p omini-tui 'quoted value'"
        );
        assert!(has_exact_fg(first, "$ ", crate::ui::theme::RUNNING));
        assert!(has_exact_fg(first, "cargo", crate::ui::theme::RUNNING));
        assert!(has_exact_fg(first, "test", crate::ui::theme::TEXT));
        assert!(has_exact_fg(first, "-p", crate::ui::theme::ERROR));
        assert!(has_exact_fg(first, "omini-tui", crate::ui::theme::TEXT));
        assert!(has_exact_fg(
            first,
            "'quoted value'",
            crate::ui::theme::SUCCESS
        ));
    }
}

// ===========================================================================
// 交互选择页
// ===========================================================================
