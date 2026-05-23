use super::*;

const PERMISSION_DRAWER_MAX_HEIGHT: u16 = 18;
const LARGE_PERMISSION_DRAWER_MAX_HEIGHT: u16 = 50;
const USER_INPUT_NONE_LABEL: &str = "以上都不是";
const USER_INPUT_NONE_DESCRIPTION: &str = "可按 Tab 在备注中补充说明。";
const USER_INPUT_NOTE_PREFIX: &str = "› ";
const USER_INPUT_NOTE_PLACEHOLDER: &str = "添加备注";

struct DrawerLines {
    lines: Vec<Line<'static>>,
    note_line: Option<Line<'static>>,
    note_cursor_column: Option<usize>,
}

struct PermissionDrawerLinesInput<'a> {
    request: &'a ToolPauseRequest,
    tool_use: Option<&'a ToolUseBlock>,
    content_width: usize,
    project_dir: Option<&'a Path>,
    question_index: usize,
    user_input_selected: usize,
    current_user_input_note: &'a str,
    user_input_note_cursor: usize,
    user_input_note_mode: bool,
}
pub(super) fn render_permission_drawer(
    state: &mut UiState,
    frame: &mut ratatui::Frame,
    area: Rect,
) {
    let Some(request) = state.active_tool_pause().cloned() else {
        state.permission_drawer_area = Rect::default();
        state.permission_drawer_body_area = Rect::default();
        state.permission_drawer_content_len = 0;
        return;
    };
    if area.width == 0 || area.height == 0 {
        state.permission_drawer_area = Rect::default();
        state.permission_drawer_body_area = Rect::default();
        state.permission_drawer_content_len = 0;
        return;
    }

    let project_dir = state.status_bar.cwd.clone();
    let preview_tool_use_id = request
        .preview_tool_use_id
        .as_deref()
        .unwrap_or(&request.tool_use_id);
    let tool_use = find_tool_use(state, preview_tool_use_id);
    let content_width = area.width.saturating_sub(6) as usize;
    let DrawerLines {
        lines,
        note_line,
        note_cursor_column,
    } = build_permission_drawer_lines(PermissionDrawerLinesInput {
        request: &request,
        tool_use,
        content_width,
        project_dir: Some(project_dir.as_path()),
        question_index: state.user_input_question_index,
        user_input_selected: state.current_user_input_selected(),
        current_user_input_note: state.current_user_input_note(),
        user_input_note_cursor: state.current_user_input_note_cursor(),
        user_input_note_mode: state.user_input_note_mode,
    });
    let fixed_header = lines.first().cloned();
    let scroll_lines: Vec<Line<'static>> = lines.into_iter().skip(1).collect();
    let note_height = if note_line.is_some() { 1 } else { 0 };
    let is_large_preview = matches!(
        request.kind,
        ToolPauseKind::Permission(PermissionPreview::Bash(_))
            | ToolPauseKind::Permission(PermissionPreview::Edit(_))
            | ToolPauseKind::Permission(PermissionPreview::Write(_))
    );

    let terminal_cap = ((area.height as f32) * 0.8).floor() as u16;
    let max_height = if is_large_preview {
        terminal_cap
            .min(LARGE_PERMISSION_DRAWER_MAX_HEIGHT)
            .min(area.height.saturating_sub(1))
            .max(1)
    } else {
        area.height
            .saturating_sub(4)
            .clamp(7, PERMISSION_DRAWER_MAX_HEIGHT)
            .min(area.height)
    };
    let desired_height = (scroll_lines.len() as u16)
        .saturating_add(8)
        .saturating_add(note_height)
        .clamp(1, max_height.min(area.height));
    let body_height = desired_height.saturating_sub(7 + note_height) as usize;
    let scroll_line_count = scroll_lines.len();
    let max_scroll = scroll_line_count.saturating_sub(body_height);
    let capped_offset = state.permission_scroll_offset.min(max_scroll);
    state.permission_scroll_offset = capped_offset;
    let scroll_y = max_scroll.saturating_sub(capped_offset);
    state.permission_drawer_content_len = scroll_lines.len();
    let visible_lines: Vec<Line<'static>> = scroll_lines
        .into_iter()
        .skip(scroll_y)
        .take(body_height)
        .collect();

    let drawer_area = Rect {
        x: area.x,
        y: area.y + area.height.saturating_sub(desired_height + 1),
        width: area.width,
        height: desired_height,
    };
    let body_area = Rect {
        x: drawer_area.x + 3,
        y: drawer_area.y + 4,
        width: drawer_area
            .width
            .saturating_sub(if max_scroll > 0 { 8 } else { 6 }),
        height: body_height as u16,
    };
    state.permission_drawer_area = drawer_area;
    state.permission_drawer_body_area = body_area;

    frame.render_widget(Clear, drawer_area);
    let accent = Color::Rgb(0x42, 0xb3, 0xc2);

    let title = match &request.kind {
        ToolPauseKind::UserInput(preview) => format!(
            " 问题 {}/{}（{} 个未回答） ",
            state.user_input_question_index + 1,
            preview.questions.len(),
            state.user_input_unanswered_count()
        ),
        ToolPauseKind::Permission(preview) => format!(" {} ", permission_drawer_title(preview)),
    };
    let divider_line = Line::from(Span::styled(
        "━".repeat(drawer_area.width.saturating_sub(1) as usize),
        Style::default().fg(accent),
    ));
    frame.render_widget(
        Paragraph::new(divider_line),
        Rect {
            x: drawer_area.x,
            y: drawer_area.y.saturating_sub(1),
            width: drawer_area.width,
            height: 1,
        },
    );
    if drawer_area.height > 1 {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                title,
                Style::default().fg(accent).add_modifier(Modifier::BOLD),
            ))),
            Rect {
                x: drawer_area.x,
                y: drawer_area.y + 1,
                width: drawer_area.width,
                height: 1,
            },
        );
    }

    if drawer_area.height > 3
        && let Some(header) = fixed_header
    {
        frame.render_widget(
            Paragraph::new(header),
            Rect {
                x: drawer_area.x + 3,
                y: drawer_area.y + 3,
                width: drawer_area.width.saturating_sub(6),
                height: 1,
            },
        );
    }

    if body_area.width > 0 && body_area.height > 0 && body_area.y < drawer_area.bottom() {
        let paragraph = Paragraph::new(Text::from(visible_lines));
        frame.render_widget(paragraph, body_area);
    }
    if max_scroll > 0 && body_area.width > 0 && body_area.height > 0 {
        render_permission_scrollbar(frame, body_area, scroll_y, scroll_line_count);
    }

    let note_area = note_line.as_ref().and_then(|_| {
        (drawer_area.height > 4).then_some(Rect {
            x: drawer_area.x + 3,
            y: drawer_area.y + drawer_area.height.saturating_sub(4),
            width: drawer_area.width.saturating_sub(6),
            height: 1,
        })
    });
    if let (Some(note_line), Some(note_area)) = (note_line, note_area) {
        frame.render_widget(Paragraph::new(note_line), note_area);
        if state.user_input_note_mode
            && let Some(note_cursor_column) = note_cursor_column
        {
            let cursor_x = note_area.x + note_cursor_column as u16;
            frame.set_cursor_position((cursor_x, note_area.y));
        }
    }

    let options = match &request.kind {
        ToolPauseKind::Permission(_) => build_permission_action_lines(state, &request),
        ToolPauseKind::UserInput(preview) => build_user_input_action_lines(state, preview),
    };
    if drawer_area.height > 2 {
        frame.render_widget(
            Paragraph::new(options),
            Rect {
                x: drawer_area.x + 3,
                y: drawer_area.y + drawer_area.height.saturating_sub(3),
                width: drawer_area.width.saturating_sub(6),
                height: 2.min(drawer_area.height.saturating_sub(2)),
            },
        );
    }
}

fn build_permission_action_lines(state: &UiState, request: &ToolPauseRequest) -> Text<'static> {
    if state.user_input_note_mode {
        return Text::from(vec![Line::from(vec![
            Span::styled(
                "Tab 或 Esc ",
                Style::default().fg(Color::Rgb(140, 145, 155)),
            ),
            Span::styled("结束备注", Style::default().fg(Color::Rgb(140, 145, 155))),
            Span::raw(" | "),
            Span::styled("Enter ", Style::default().fg(Color::Rgb(140, 145, 155))),
            Span::styled("拒绝请求", Style::default().fg(Color::Rgb(140, 145, 155))),
        ])]);
    }

    let yes_style = permission_option_style(state.permission_selected == 0);
    let no_style = permission_option_style(state.permission_selected == 1);
    let (yes_desc, no_desc) = permission_option_descriptions(request);
    let desc_style = Style::default().fg(Color::Rgb(140, 145, 155));
    Text::from(vec![
        Line::from(vec![
            Span::styled("1. ", yes_style),
            Span::styled(format!("{:<3}", "Yes"), yes_style),
            Span::raw("   "),
            Span::styled(yes_desc, desc_style),
        ]),
        Line::from(vec![
            Span::styled("2. ", no_style),
            Span::styled(format!("{:<3}", "No"), no_style),
            Span::raw("   "),
            Span::styled(format!("{no_desc} · Tab note"), desc_style),
        ]),
    ])
}

fn build_user_input_action_lines(
    state: &UiState,
    _preview: &crate::types::events::UserInputPreview,
) -> Text<'static> {
    if state.user_input_note_mode {
        Text::from(vec![Line::from(vec![
            Span::styled(
                "Tab 或 Esc ",
                Style::default().fg(Color::Rgb(140, 145, 155)),
            ),
            Span::styled("结束备注", Style::default().fg(Color::Rgb(140, 145, 155))),
            Span::raw(" | "),
            Span::styled("Enter ", Style::default().fg(Color::Rgb(140, 145, 155))),
            Span::styled("提交回答", Style::default().fg(Color::Rgb(140, 145, 155))),
        ])])
    } else {
        Text::from(vec![Line::from(vec![
            Span::styled(
                "Tab 添加备注",
                Style::default()
                    .fg(Color::Rgb(0x42, 0xd9, 0xe8))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" | "),
            Span::styled(
                "Enter 提交回答",
                Style::default().fg(Color::Rgb(140, 145, 155)),
            ),
            Span::raw(" | "),
            Span::styled(
                "←/→ 切换问题",
                Style::default().fg(Color::Rgb(140, 145, 155)),
            ),
            Span::raw(" | "),
            Span::styled("Esc 中断", Style::default().fg(Color::Rgb(140, 145, 155))),
        ])])
    }
}

fn render_permission_scrollbar(
    frame: &mut ratatui::Frame,
    body_area: Rect,
    scroll_y: usize,
    total_lines: usize,
) {
    let height = body_area.height as usize;
    if height == 0 || total_lines <= height {
        return;
    }

    let max_scroll = total_lines.saturating_sub(height);
    let thumb_height = (height.saturating_mul(height) / total_lines).clamp(1, height);
    let thumb_range = height.saturating_sub(thumb_height);
    let thumb_y = if max_scroll == 0 {
        0
    } else {
        scroll_y.saturating_mul(thumb_range) / max_scroll
    };
    let x = body_area.x + body_area.width + 1;
    let track_style = Style::default().fg(Color::Rgb(70, 75, 86));
    let thumb_style = Style::default().fg(Color::Rgb(140, 145, 155));

    for i in 0..height {
        let is_thumb = i >= thumb_y && i < thumb_y + thumb_height;
        let symbol = if is_thumb { "┃" } else { "│" };
        let style = if is_thumb { thumb_style } else { track_style };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(symbol, style))),
            Rect {
                x,
                y: body_area.y + i as u16,
                width: 1,
                height: 1,
            },
        );
    }
}

fn permission_option_style(selected: bool) -> Style {
    let style = Style::default();
    if selected {
        style
            .fg(Color::Rgb(0x42, 0xd9, 0xe8))
            .add_modifier(Modifier::BOLD)
    } else {
        style
    }
}

fn permission_option_descriptions(request: &ToolPauseRequest) -> (&'static str, &'static str) {
    match &request.kind {
        ToolPauseKind::Permission(PermissionPreview::Bash(_)) => ("run command", "skip command"),
        ToolPauseKind::Permission(PermissionPreview::Edit(_)) => {
            ("apply changes", "reject changes")
        }
        ToolPauseKind::Permission(PermissionPreview::Write(_)) => ("write file", "reject write"),
        ToolPauseKind::Permission(PermissionPreview::Read(_)) => ("read file", "skip read"),
        ToolPauseKind::Permission(PermissionPreview::Search(_)) => ("search path", "skip search"),
        ToolPauseKind::Permission(PermissionPreview::Custom { .. }) => ("allow tool", "deny tool"),
        ToolPauseKind::UserInput(_) => ("提交回答", "取消请求"),
    }
}

fn build_permission_drawer_lines(input: PermissionDrawerLinesInput<'_>) -> DrawerLines {
    let PermissionDrawerLinesInput {
        request,
        tool_use,
        content_width,
        project_dir,
        question_index,
        user_input_selected,
        current_user_input_note,
        user_input_note_cursor,
        user_input_note_mode,
    } = input;

    let mut drawer = match &request.kind {
        ToolPauseKind::Permission(PermissionPreview::Bash(preview)) => {
            let mut lines = Vec::new();
            lines.push(Line::from(""));
            lines.push(Line::from(""));

            if let Some(description) = &preview.description
                && !description.trim().is_empty()
            {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled("说明：", Style::default().fg(Color::Rgb(140, 145, 155))),
                    Span::styled(
                        description.trim().to_string(),
                        Style::default().fg(Color::Rgb(220, 220, 225)),
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
                note_line: None,
                note_cursor_column: None,
            }
        }
        ToolPauseKind::Permission(PermissionPreview::Edit(_)) => {
            let lines = if let Some(tool_use) = tool_use {
                render_tool(tool_use, None, Some(request), content_width, project_dir)
            } else {
                vec![Line::from(Span::styled(
                    "缺少编辑预览所需的工具输入",
                    Style::default().fg(Color::Rgb(255, 100, 100)),
                ))]
            };
            DrawerLines {
                lines,
                note_line: None,
                note_cursor_column: None,
            }
        }
        ToolPauseKind::Permission(PermissionPreview::Write(_)) => {
            let lines = if let Some(tool_use) = tool_use {
                render_tool(tool_use, None, Some(request), content_width, project_dir)
            } else {
                vec![Line::from(Span::styled(
                    "缺少写入预览所需的工具输入",
                    Style::default().fg(Color::Rgb(255, 100, 100)),
                ))]
            };
            DrawerLines {
                lines,
                note_line: None,
                note_cursor_column: None,
            }
        }
        ToolPauseKind::Permission(PermissionPreview::Read(preview)) => {
            let lines = if let Some(tool_use) = tool_use {
                render_tool(tool_use, None, Some(request), content_width, project_dir)
            } else {
                vec![Line::from(vec![
                    Span::raw("  "),
                    Span::styled("路径：", Style::default().fg(Color::Rgb(140, 145, 155))),
                    Span::styled(
                        display_path(&preview.file_path, project_dir),
                        Style::default().fg(Color::Rgb(220, 220, 225)),
                    ),
                ])]
            };
            DrawerLines {
                lines,
                note_line: None,
                note_cursor_column: None,
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
                Span::styled("模式：", Style::default().fg(Color::Rgb(140, 145, 155))),
                Span::styled(
                    mode.to_string(),
                    Style::default().fg(Color::Rgb(220, 220, 225)),
                ),
            ]));
            if !preview.query.trim().is_empty() {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled("查询：", Style::default().fg(Color::Rgb(140, 145, 155))),
                    Span::styled(
                        preview.query.trim().to_string(),
                        Style::default().fg(Color::Rgb(220, 220, 225)),
                    ),
                ]));
            }
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled("路径：", Style::default().fg(Color::Rgb(140, 145, 155))),
                Span::styled(
                    display_path(&preview.path, project_dir),
                    Style::default().fg(Color::Rgb(220, 220, 225)),
                ),
            ]));
            DrawerLines {
                lines,
                note_line: None,
                note_cursor_column: None,
            }
        }
        ToolPauseKind::Permission(_preview) => DrawerLines {
            lines: vec![Line::from("")],
            note_line: None,
            note_cursor_column: None,
        },
        ToolPauseKind::UserInput(preview) => {
            let Some(question) = preview.questions.get(question_index) else {
                return DrawerLines {
                    lines: vec![Line::from("缺少问题")],
                    note_line: None,
                    note_cursor_column: None,
                };
            };
            let mut lines = Vec::new();
            lines.extend(user_input_question_lines(question, content_width));
            lines.push(Line::from(""));
            for (idx, option) in question.options.iter().enumerate() {
                let selected = idx == user_input_selected;
                lines.push(user_input_option_line(
                    selected,
                    &format!("{}. {}", idx + 1, option.label),
                    &option.description,
                ));
            }
            lines.push(user_input_option_line(
                user_input_selected == question.options.len(),
                &format!("{}. {}", question.options.len() + 1, USER_INPUT_NONE_LABEL),
                USER_INPUT_NONE_DESCRIPTION,
            ));
            let mut drawer = DrawerLines {
                lines,
                note_line: None,
                note_cursor_column: None,
            };
            set_note_line(
                &mut drawer,
                current_user_input_note,
                user_input_note_cursor,
                user_input_note_mode,
            );
            drawer
        }
    };
    if matches!(&request.kind, ToolPauseKind::Permission(_)) {
        set_note_line(
            &mut drawer,
            current_user_input_note,
            user_input_note_cursor,
            user_input_note_mode,
        );
    }
    add_permission_source_line(&mut drawer, request);
    drawer
}

fn set_note_line(drawer: &mut DrawerLines, note: &str, cursor: usize, editing: bool) {
    if !editing && note.is_empty() {
        return;
    }

    drawer.note_line = Some(user_input_note_line(note, editing));
    drawer.note_cursor_column = Some(user_input_note_cursor_column(note, cursor));
}

fn add_permission_source_line(drawer: &mut DrawerLines, request: &ToolPauseRequest) {
    if drawer.lines.is_empty() {
        return;
    };

    let mut insert_at = 1usize;
    if let Some(label) = request.source_agent_label.as_deref() {
        drawer.lines.insert(
            insert_at,
            Line::from(vec![
                Span::raw("  "),
                Span::styled("来源：", Style::default().fg(Color::Rgb(140, 145, 155))),
                Span::styled(
                    label.to_string(),
                    Style::default().fg(Color::Rgb(220, 220, 225)),
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
                Span::styled("规则：", Style::default().fg(Color::Rgb(140, 145, 155))),
                Span::styled(
                    source.decision.clone(),
                    Style::default().fg(Color::Rgb(220, 220, 225)),
                ),
            ]),
        );
        drawer.lines.insert(
            insert_at + 1,
            Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    source.source.clone(),
                    Style::default().fg(Color::Rgb(165, 172, 182)),
                ),
                Span::styled(" -> ", Style::default().fg(Color::Rgb(95, 101, 113))),
                Span::styled(
                    source.rule.clone(),
                    Style::default().fg(Color::Rgb(165, 172, 182)),
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

fn bash_permission_command_lines(command: &str, content_width: usize) -> Vec<Line<'static>> {
    let prompt_style = Style::default()
        .fg(Color::Rgb(0x50, 0xc8, 0x78))
        .add_modifier(Modifier::BOLD);
    let command_style = Style::default().fg(Color::Rgb(220, 220, 225));
    let prefix = "  ";
    let prompt = "$ ";
    let continuation = "    ";
    let command_width = content_width
        .saturating_sub(prefix.width())
        .saturating_sub(prompt.width())
        .max(1);
    let wrapped = wrap_preserving_display_width(command, command_width);

    if wrapped.is_empty() {
        return vec![Line::from(vec![
            Span::raw(prefix),
            Span::styled(prompt, prompt_style),
        ])];
    }

    wrapped
        .into_iter()
        .enumerate()
        .map(|(idx, segment)| {
            if idx == 0 {
                Line::from(vec![
                    Span::raw(prefix),
                    Span::styled(prompt, prompt_style),
                    Span::styled(segment, command_style),
                ])
            } else {
                Line::from(vec![
                    Span::raw(continuation),
                    Span::styled(segment, command_style),
                ])
            }
        })
        .collect()
}

fn wrap_preserving_display_width(text: &str, max_width: usize) -> Vec<String> {
    let max_width = max_width.max(1);
    let mut lines = Vec::new();

    for source_line in text.split('\n') {
        if source_line.is_empty() {
            lines.push(String::new());
            continue;
        }

        let mut current = String::new();
        let mut current_width = 0;
        for ch in source_line.chars() {
            let char_width = ch.width().unwrap_or(0);
            if current_width > 0 && current_width + char_width > max_width {
                lines.push(current);
                current = String::new();
                current_width = 0;
            }
            current.push(ch);
            current_width += char_width;
        }
        lines.push(current);
    }

    lines
}

fn user_input_note_line(note: &str, editing: bool) -> Line<'static> {
    let marker_style = if editing {
        Style::default()
            .fg(Color::Rgb(0x42, 0xd9, 0xe8))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Rgb(140, 145, 155))
    };
    let value = if note.is_empty() {
        USER_INPUT_NOTE_PLACEHOLDER
    } else {
        note
    };
    let value_style = if note.is_empty() {
        Style::default().fg(Color::Rgb(140, 145, 155))
    } else {
        Style::default().fg(Color::Rgb(220, 220, 225))
    };
    Line::from(vec![
        Span::styled(USER_INPUT_NOTE_PREFIX, marker_style),
        Span::styled(value.to_string(), value_style),
    ])
}

fn user_input_question_lines(
    question: &crate::types::events::UserInputQuestion,
    content_width: usize,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(Span::styled(
        question.header.clone(),
        Style::default().fg(Color::Rgb(140, 145, 155)),
    ))];
    lines.extend(
        crate::widgets::word_wrap(&question.question, content_width)
            .into_iter()
            .map(|line| {
                Line::from(Span::styled(
                    line,
                    Style::default()
                        .fg(Color::Rgb(0x42, 0xd9, 0xe8))
                        .add_modifier(Modifier::BOLD),
                ))
            }),
    );
    lines
}

fn user_input_note_cursor_column(note: &str, cursor_char: usize) -> usize {
    USER_INPUT_NOTE_PREFIX.width()
        + note
            .chars()
            .take(cursor_char)
            .map(|c| c.width().unwrap_or(0))
            .sum::<usize>()
}

fn user_input_option_line(selected: bool, label: &str, description: &str) -> Line<'static> {
    let marker_style = if selected {
        Style::default()
            .fg(Color::Rgb(0x42, 0xd9, 0xe8))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Rgb(85, 92, 105))
    };
    let label_style = if selected {
        Style::default()
            .fg(Color::Rgb(0x42, 0xd9, 0xe8))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Rgb(220, 220, 225))
    };
    Line::from(vec![
        Span::styled(if selected { "› " } else { "  " }, marker_style),
        Span::styled(label.to_string(), label_style),
        Span::raw("   "),
        Span::styled(
            description.to_string(),
            Style::default().fg(Color::Rgb(140, 145, 155)),
        ),
    ])
}

fn permission_drawer_title(preview: &PermissionPreview) -> &'static str {
    match preview {
        PermissionPreview::Bash(_) => "Run Command",
        PermissionPreview::Edit(_) => "Edit File",
        PermissionPreview::Write(_) => "Write File",
        PermissionPreview::Read(_) => "Read File",
        PermissionPreview::Search(_) => "Search Files",
        PermissionPreview::Custom { .. } => "Tool Permission",
    }
}

fn find_tool_use<'a>(state: &'a UiState, tool_use_id: &str) -> Option<&'a ToolUseBlock> {
    state
        .pending_assistant
        .iter()
        .flat_map(|m| m.content.iter())
        .chain(
            state
                .messages
                .iter()
                .filter_map(UiMessage::as_message)
                .flat_map(|m| m.content.iter()),
        )
        .chain(
            state
                .subagents
                .values()
                .flat_map(|node| node.messages.iter())
                .flat_map(|m| m.content.iter()),
        )
        .find_map(|block| match block {
            ContentBlock::ToolUse(tu) if tu.id == tool_use_id => Some(tu),
            _ => None,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn permission_request() -> ToolPauseRequest {
        ToolPauseRequest {
            tool_use_id: "tool_1".to_string(),
            preview_tool_use_id: None,
            tool_name: "write".to_string(),
            permission_source: None,
            source_session_id: None,
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

        let note_line = drawer.note_line.as_ref().expect("note line should render");
        assert!(line_text(note_line).contains("Use English comments"));
        assert!(
            !drawer
                .lines
                .iter()
                .any(|line| line_text(line).contains("Use English comments"))
        );
    }

    #[test]
    fn search_permission_renders_query_and_path() {
        let request = ToolPauseRequest {
            tool_use_id: "search-1".to_string(),
            preview_tool_use_id: None,
            tool_name: "search".to_string(),
            permission_source: None,
            source_session_id: None,
            source_agent_label: None,
            kind: ToolPauseKind::Permission(PermissionPreview::Search(
                crate::types::events::SearchPermissionPreview {
                    query: "POST /api/v1/skills".to_string(),
                    mode: "content".to_string(),
                    path: "/home/zzx/.omini/skills/uumit-agent".to_string(),
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
        assert!(rendered.contains("~/.omini/skills/uumit-agent"));
    }

    #[test]
    fn bash_permission_wraps_long_command_without_truncating() {
        let command = "cargo test -p omini-tui permission_drawer_with_a_very_long_filter_name_that_exceeds_the_drawer_width -- --nocapture";
        let request = ToolPauseRequest {
            tool_use_id: "bash-1".to_string(),
            preview_tool_use_id: None,
            tool_name: "bash".to_string(),
            permission_source: None,
            source_session_id: None,
            source_agent_label: None,
            kind: ToolPauseKind::Permission(PermissionPreview::Bash(
                crate::types::events::BashPermissionPreview {
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
}

// ===========================================================================
// 交互选择页
// ===========================================================================
