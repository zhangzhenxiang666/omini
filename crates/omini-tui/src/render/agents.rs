use super::*;

pub(super) fn render_agents_panel(
    state: &mut UiState,
    frame: &mut ratatui::Frame,
    area: Rect,
    manager: &AgentManagerState,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let max_panel_height = ((area.height as f32) * 0.75).round() as u16;
    let max_panel_height = max_panel_height.max(28).min(area.height);
    let max_content_height = max_panel_height.saturating_sub(3) as usize;
    let natural_lines =
        build_agents_lines(manager, area.width.saturating_sub(4) as usize, usize::MAX);
    let content_height = natural_lines.len().clamp(5, max_content_height.max(1));
    let panel_height = (content_height as u16)
        .saturating_add(3)
        .min(max_panel_height);
    let panel_area = Rect {
        x: area.x,
        y: area.y + area.height.saturating_sub(panel_height),
        width: area.width,
        height: panel_height,
    };
    frame.render_widget(Clear, panel_area);

    let accent = Color::Rgb(0x42, 0xd9, 0xe8);
    let mut divider_line = Line::from(Span::styled(
        "━".repeat(panel_area.width.saturating_sub(1) as usize),
        Style::default().fg(accent),
    ));
    let divider_area = Rect {
        x: panel_area.x,
        y: panel_area.y.saturating_sub(1),
        width: panel_area.width,
        height: 1,
    };
    register_and_highlight_lines(state, divider_area, std::slice::from_mut(&mut divider_line));
    frame.render_widget(Paragraph::new(divider_line), divider_area);

    let content_area = Rect {
        x: panel_area.x + 2,
        y: panel_area.y + 1,
        width: panel_area.width.saturating_sub(4),
        height: panel_area.height.saturating_sub(3),
    };
    let footer_area = Rect {
        x: panel_area.x + 2,
        y: panel_area.y + panel_area.height.saturating_sub(1),
        width: panel_area.width.saturating_sub(4),
        height: 1,
    };
    let lines = build_agents_lines(
        manager,
        content_area.width as usize,
        content_area.height as usize,
    );
    let mut rendered = lines;
    if content_area.width > 0 && content_area.height > 0 && content_area.y < panel_area.bottom() {
        register_and_highlight_lines(state, content_area, &mut rendered);
        frame.render_widget(Paragraph::new(Text::from(rendered)), content_area);
    }

    let mut footer = agents_footer_hint(manager);
    if footer_area.width > 0 {
        register_and_highlight_lines(state, footer_area, std::slice::from_mut(&mut footer));
        frame.render_widget(Paragraph::new(footer), footer_area);
    }

    if content_area.width > 0
        && content_area.height > 0
        && let Some((line_idx, col)) = agent_editor_cursor(
            manager,
            content_area.width as usize,
            content_area.height as usize,
        )
    {
        let cursor_y = content_area.y.saturating_add(line_idx as u16).min(
            content_area
                .y
                .saturating_add(content_area.height.saturating_sub(1)),
        );
        let cursor_x = content_area.x.saturating_add(col as u16).min(
            content_area
                .x
                .saturating_add(content_area.width.saturating_sub(1)),
        );
        frame.set_cursor_position((cursor_x, cursor_y));
    }
}

fn build_agents_lines(
    manager: &AgentManagerState,
    width: usize,
    content_height: usize,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(
            "Agents",
            Style::default()
                .fg(Color::Rgb(0x42, 0xd9, 0xe8))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "  管理内置、项目级和用户级 agent",
            Style::default().fg(Color::Rgb(140, 145, 155)),
        ),
    ]));
    lines.push(Line::from(""));
    match &manager.view {
        AgentManagerView::List => build_agents_list_lines(manager, width, &mut lines),
        AgentManagerView::Detail(idx) => {
            build_agent_detail_lines(manager, *idx, width, content_height, &mut lines)
        }
        AgentManagerView::EditMenu => build_agent_edit_menu_lines(manager, &mut lines),
        AgentManagerView::EditMetadata => {
            build_agent_edit_metadata_lines(manager, width, content_height, &mut lines)
        }
        AgentManagerView::EditTools => {
            let remaining_height = content_height.saturating_sub(lines.len());
            build_agent_tools_lines(manager, remaining_height, &mut lines);
        }
        AgentManagerView::EditModel => {
            let remaining_height = content_height.saturating_sub(lines.len());
            build_agent_model_lines(manager, remaining_height, &mut lines);
        }
        AgentManagerView::Create(step) => {
            build_agent_create_lines(manager, *step, width, content_height, &mut lines)
        }
        AgentManagerView::GeneratedPreview => {
            build_agent_editor_lines(manager, width, content_height, &mut lines)
        }
        AgentManagerView::Generate => {
            build_agent_generate_lines(manager, width, content_height, false, &mut lines)
        }
        AgentManagerView::Generating(_) => build_agent_generating_lines(manager, width, &mut lines),
        AgentManagerView::ConfirmDelete(idx) => {
            if let Some(record) = manager.records.get(*idx) {
                lines.push(Line::from(format!("确认删除 agent: {}", record.name)));
                lines.push(Line::from(format!(
                    "来源: {}  路径: {}",
                    record.source_kind.label(),
                    record
                        .path
                        .as_ref()
                        .map(|path| path.display().to_string())
                        .unwrap_or_default()
                )));
                lines.push(Line::from(""));
            }
        }
    }
    if let Some(message) = &manager.message {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            message.clone(),
            Style::default().fg(Color::Rgb(220, 185, 145)),
        )));
    }
    lines
}

fn agents_footer_hint(manager: &AgentManagerState) -> Line<'static> {
    let text = match &manager.view {
        AgentManagerView::List => "↑↓ 选择 · Enter 确认 · Esc 关闭",
        AgentManagerView::Detail(_) => "↑↓ 选择操作 · Enter 确认 · Esc 返回",
        AgentManagerView::EditMenu => "↑↓ 选择 · Enter 确认 · Esc 返回",
        AgentManagerView::EditMetadata => "Tab 切换字段 · Enter/Esc 自动保存并返回菜单",
        AgentManagerView::EditTools => "↑↓ 选择工具 · Space 开关 · Enter/Esc 自动保存并返回菜单",
        AgentManagerView::EditModel => "↑↓ 选择模型 · Enter/Esc 自动保存并返回菜单",
        AgentManagerView::Create(step) => match step {
            AgentCreateStep::Scope => "↑↓ 选择 · Enter 确认 · Esc 返回",
            AgentCreateStep::Tools => "↑↓ 选择 · Space 开关 · Enter 确认 · Esc 返回",
            AgentCreateStep::Model => "↑↓ 选择 · Enter 确认 · Esc 返回",
            AgentCreateStep::Method => "↑↓ 选择 · Enter 确认 · Esc 上一步",
            AgentCreateStep::ManualName => "输入名称 · Enter 下一步 · Esc 上一步",
            AgentCreateStep::ManualDescription => "输入描述 · Enter 下一步 · Esc 上一步",
            AgentCreateStep::ManualInstructions => "输入系统指令 · Enter 保存 · Esc 上一步",
            AgentCreateStep::GenerateDescription => "输入预期 agent 描述 · Enter 生成 · Esc 返回",
        },
        AgentManagerView::GeneratedPreview => "Tab 切换字段 · Enter 保存 · Esc 返回",
        AgentManagerView::Generate => "输入预期 agent 描述 · Enter 生成 · Esc 返回",
        AgentManagerView::Generating(_) => "正在生成 agent，完成后会自动进入预览",
        AgentManagerView::ConfirmDelete(_) => "Enter 删除 · Esc 返回",
    };
    hint(text)
}

fn agent_editor_cursor(
    manager: &AgentManagerState,
    width: usize,
    content_height: usize,
) -> Option<(usize, usize)> {
    const PANEL_PREFIX_LINES: usize = 2;
    let input_width = input_box_inner_width(width);
    match &manager.view {
        AgentManagerView::GeneratedPreview => match manager.draft.field {
            AgentEditorField::Name => Some((
                PANEL_PREFIX_LINES + agent_editor_cursor_prefix_lines(manager, 0),
                input_box_cursor_col(&manager.draft.name, manager.draft.cursor),
            )),
            AgentEditorField::Description => Some((
                {
                    let window = editable_text_window(
                        &manager.draft.description,
                        manager.draft.cursor,
                        input_width,
                        AGENT_DESCRIPTION_MAX_LINES,
                        "未填写",
                    );
                    PANEL_PREFIX_LINES
                        + agent_editor_description_cursor_prefix_lines(manager)
                        + window.cursor_line
                },
                {
                    let window = editable_text_window(
                        &manager.draft.description,
                        manager.draft.cursor,
                        input_width,
                        AGENT_DESCRIPTION_MAX_LINES,
                        "未填写",
                    );
                    2 + window.cursor_col
                },
            )),
            AgentEditorField::Instructions => {
                let content_line_idx = PANEL_PREFIX_LINES
                    + agent_editor_instructions_cursor_prefix_lines(manager, input_width);
                let window = editable_text_window(
                    &manager.draft.instructions,
                    manager.draft.cursor,
                    input_width,
                    cursor_text_box_max_lines(content_line_idx, content_height, 10),
                    "输入 agent 的系统指令。",
                );
                Some((content_line_idx + window.cursor_line, 2 + window.cursor_col))
            }
            AgentEditorField::Tools | AgentEditorField::Model => None,
            AgentEditorField::GenerateDescription => None,
        },
        AgentManagerView::EditMetadata => match manager.draft.field {
            AgentEditorField::Name => Some((
                PANEL_PREFIX_LINES + 5,
                input_box_cursor_col(&manager.draft.name, manager.draft.cursor),
            )),
            AgentEditorField::Description => {
                let window = editable_text_window(
                    &manager.draft.description,
                    manager.draft.cursor,
                    input_width,
                    AGENT_DESCRIPTION_MAX_LINES,
                    "未填写",
                );
                Some((
                    PANEL_PREFIX_LINES + 9 + window.cursor_line,
                    2 + window.cursor_col,
                ))
            }
            AgentEditorField::Instructions => {
                let description_lines = editable_text_window(
                    &manager.draft.description,
                    manager.draft.description.chars().count(),
                    input_width,
                    AGENT_DESCRIPTION_MAX_LINES,
                    "未填写",
                )
                .lines
                .len();
                let content_line_idx = PANEL_PREFIX_LINES + 12 + description_lines;
                let window = editable_text_window(
                    &manager.draft.instructions,
                    manager.draft.cursor,
                    input_width,
                    cursor_text_box_max_lines(
                        content_line_idx,
                        content_height,
                        AGENT_EDIT_CONTENT_INSTRUCTIONS_MAX_LINES,
                    ),
                    "输入 agent 的系统指令。",
                );
                Some((content_line_idx + window.cursor_line, 2 + window.cursor_col))
            }
            _ => None,
        },
        AgentManagerView::Generate => {
            let content_line_idx =
                PANEL_PREFIX_LINES + agent_generate_cursor_prefix_lines(manager, false);
            let window = editable_text_window(
                &manager.draft.generated_description,
                manager.draft.cursor,
                input_width,
                cursor_text_box_max_lines(content_line_idx, content_height, 12),
                "例如：擅长翻译 Rust 代码注释，只读取必要文件，保持代码不变。",
            );
            Some((content_line_idx + window.cursor_line, 2 + window.cursor_col))
        }
        AgentManagerView::Create(step) => match step {
            AgentCreateStep::ManualName => Some((
                PANEL_PREFIX_LINES + agent_manual_create_field_cursor_prefix_lines(manager),
                input_box_cursor_col(&manager.draft.name, manager.draft.cursor),
            )),
            AgentCreateStep::ManualDescription => {
                let window = editable_text_window(
                    &manager.draft.description,
                    manager.draft.cursor,
                    input_width,
                    AGENT_DESCRIPTION_MAX_LINES,
                    "这个 agent 适合做什么",
                );
                Some((
                    PANEL_PREFIX_LINES
                        + agent_manual_create_field_cursor_prefix_lines(manager)
                        + window.cursor_line,
                    2 + window.cursor_col,
                ))
            }
            AgentCreateStep::ManualInstructions => {
                let content_line_idx =
                    PANEL_PREFIX_LINES + agent_manual_create_field_cursor_prefix_lines(manager);
                let window = editable_text_window(
                    &manager.draft.instructions,
                    manager.draft.cursor,
                    input_width,
                    cursor_text_box_max_lines(content_line_idx, content_height, 12),
                    "输入 agent 的系统指令。",
                );
                Some((content_line_idx + window.cursor_line, 2 + window.cursor_col))
            }
            AgentCreateStep::GenerateDescription => {
                let content_line_idx =
                    PANEL_PREFIX_LINES + agent_generate_cursor_prefix_lines(manager, true);
                let window = editable_text_window(
                    &manager.draft.generated_description,
                    manager.draft.cursor,
                    input_width,
                    cursor_text_box_max_lines(content_line_idx, content_height, 12),
                    "例如：擅长翻译 Rust 代码注释，只读取必要文件，保持代码不变。",
                );
                Some((content_line_idx + window.cursor_line, 2 + window.cursor_col))
            }
            _ => None,
        },
        _ => None,
    }
}

fn agent_editor_cursor_prefix_lines(manager: &AgentManagerState, field_index: usize) -> usize {
    7 + agent_tool_summary_line_count(&manager.draft.tools, &manager.draft.disallow_tools)
        + field_index * 4
}

fn agent_manual_create_field_cursor_prefix_lines(manager: &AgentManagerState) -> usize {
    // Inside build_agent_create_lines before the editable content row:
    // tabs + blank + summary(source/tools/model/blank) + label + top border.
    2 + 1
        + agent_tool_summary_line_count(&manager.draft.tools, &manager.draft.disallow_tools)
        + 1
        + 1
        + 1
        + 1
}

const AGENT_DESCRIPTION_MAX_LINES: usize = 3;

fn agent_editor_description_cursor_prefix_lines(manager: &AgentManagerState) -> usize {
    agent_editor_cursor_prefix_lines(manager, 1)
}

fn agent_editor_instructions_cursor_prefix_lines(
    manager: &AgentManagerState,
    input_width: usize,
) -> usize {
    agent_editor_instructions_lines_before_box(manager, input_width) + 2
}

fn agent_editor_instructions_lines_before_box(
    manager: &AgentManagerState,
    input_width: usize,
) -> usize {
    let summary_lines =
        agent_tool_summary_line_count(&manager.draft.tools, &manager.draft.disallow_tools);
    let description_cursor = if manager.draft.field == AgentEditorField::Description {
        manager.draft.cursor
    } else {
        manager.draft.description.chars().count()
    };
    let description_lines = editable_text_window(
        &manager.draft.description,
        description_cursor,
        input_width,
        AGENT_DESCRIPTION_MAX_LINES,
        "未填写",
    )
    .lines
    .len();
    12 + summary_lines + description_lines
}

fn agent_generate_cursor_prefix_lines(manager: &AgentManagerState, create_flow: bool) -> usize {
    let create_tabs = if create_flow { 2 } else { 0 };
    let before_box_content =
        5 + agent_tool_summary_line_count(&manager.draft.tools, &manager.draft.disallow_tools);
    create_tabs + before_box_content
}

fn cursor_text_box_max_lines(
    content_line_idx: usize,
    content_height: usize,
    preferred: usize,
) -> usize {
    text_box_max_lines(
        content_line_idx.saturating_sub(2),
        content_height,
        preferred,
    )
}

fn input_box_cursor_col(value: &str, cursor: usize) -> usize {
    let prefix: String = value.chars().take(cursor).collect();
    2 + prefix.width()
}

pub(super) fn input_box_inner_width(width: usize) -> usize {
    input_box_width(width).saturating_sub(4).max(1)
}

struct EditableTextWindow {
    lines: Vec<String>,
    cursor_line: usize,
    cursor_col: usize,
}

#[derive(Clone)]
struct EditableTextLine {
    start_char: usize,
    end_char: usize,
    text: String,
}

fn editable_text_window(
    text: &str,
    cursor: usize,
    width: usize,
    max_lines: usize,
    placeholder: &str,
) -> EditableTextWindow {
    if text.is_empty() {
        return EditableTextWindow {
            lines: crate::widgets::word_wrap(placeholder, width)
                .into_iter()
                .take(max_lines.max(1))
                .collect(),
            cursor_line: 0,
            cursor_col: 0,
        };
    }

    let max_lines = max_lines.max(1);
    let lines = editable_text_lines(text, width);
    let (cursor_line_abs, cursor_col) = editable_text_cursor(text, cursor, &lines);
    let start = cursor_line_abs.saturating_sub(max_lines - 1);
    let visible = lines
        .into_iter()
        .skip(start)
        .take(max_lines)
        .map(|line| line.text)
        .collect::<Vec<_>>();

    EditableTextWindow {
        lines: visible,
        cursor_line: cursor_line_abs.saturating_sub(start),
        cursor_col,
    }
}

fn editable_text_cursor(text: &str, cursor: usize, lines: &[EditableTextLine]) -> (usize, usize) {
    for (line_idx, line) in lines.iter().enumerate() {
        if cursor >= line.start_char && cursor <= line.end_char {
            return (
                line_idx,
                text.chars()
                    .skip(line.start_char)
                    .take(cursor.saturating_sub(line.start_char))
                    .map(char_display_width)
                    .sum(),
            );
        }
    }
    lines
        .last()
        .map(|line| {
            (
                lines.len().saturating_sub(1),
                text.chars()
                    .skip(line.start_char)
                    .take(line.end_char.saturating_sub(line.start_char))
                    .map(char_display_width)
                    .sum(),
            )
        })
        .unwrap_or((0, 0))
}

fn editable_text_lines(text: &str, width: usize) -> Vec<EditableTextLine> {
    let chars = text.chars().collect::<Vec<_>>();
    if chars.is_empty() {
        return vec![EditableTextLine {
            start_char: 0,
            end_char: 0,
            text: String::new(),
        }];
    }

    let width = width.max(1);
    let mut lines = Vec::new();
    let mut start = 0usize;
    let mut line_width = 0usize;
    for (idx, ch) in chars.iter().copied().enumerate() {
        if ch == '\n' {
            lines.push(editable_text_line(&chars, start, idx));
            start = idx + 1;
            line_width = 0;
            continue;
        }

        let ch_width = char_display_width(ch);
        if line_width > 0 && line_width + ch_width > width {
            lines.push(editable_text_line(&chars, start, idx));
            start = idx;
            line_width = 0;
        }
        line_width += ch_width;
    }
    lines.push(editable_text_line(&chars, start, chars.len()));
    lines
}

fn editable_text_line(chars: &[char], start_char: usize, end_char: usize) -> EditableTextLine {
    EditableTextLine {
        start_char,
        end_char,
        text: chars[start_char..end_char].iter().collect(),
    }
}

fn char_display_width(ch: char) -> usize {
    unicode_width::UnicodeWidthChar::width(ch)
        .unwrap_or(0)
        .max(1)
}

fn build_agents_list_lines(
    manager: &AgentManagerState,
    width: usize,
    lines: &mut Vec<Line<'static>>,
) {
    let create_selected = manager.selected == 0;
    let create_style = if create_selected {
        Style::default()
            .fg(Color::Rgb(0x42, 0xd9, 0xe8))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Rgb(0xa5, 0xac, 0xb6))
    };
    lines.push(Line::from(Span::styled(
        format!(
            "{} {} {} {}",
            if create_selected { "❯" } else { " " },
            pad_display_width("创建agent", 22),
            pad_display_width("新建", 12),
            "手动创建或根据描述生成 agent"
        ),
        create_style,
    )));
    lines.push(Line::from(""));

    if manager.records.is_empty() {
        lines.push(Line::from(Span::styled(
            "  暂无自定义 agent；内置 agent 会在可用时显示在这里。",
            Style::default().fg(Color::Rgb(140, 145, 155)),
        )));
        lines.push(Line::from(""));
        return;
    }
    let visible = 18usize;
    let record_selected = manager.selected.saturating_sub(1);
    let start = record_selected.saturating_sub(visible / 2);
    let end = (start + visible).min(manager.records.len());
    let mut last_kind = None;
    for idx in start..end {
        let record = &manager.records[idx];
        if last_kind != Some(record.source_kind) {
            lines.push(Line::from(Span::styled(
                format!("{} agent", record.source_kind.label()),
                Style::default()
                    .fg(Color::Rgb(140, 145, 155))
                    .add_modifier(Modifier::BOLD),
            )));
            last_kind = Some(record.source_kind);
        }
        let selected = manager.selected > 0 && idx == record_selected;
        let marker = if selected { "❯" } else { " " };
        let lock = if record.editable { "" } else { " readonly" };
        let name = truncate_str(&record.name, 22);
        let source = record.source_kind.label();
        let text = format!(
            "{} {} {} {}{}",
            marker,
            pad_display_width(&name, 22),
            pad_display_width(source, 12),
            truncate_str(&record.description, width.saturating_sub(48)),
            lock
        );
        let style = if selected {
            Style::default()
                .fg(Color::Rgb(0x42, 0xd9, 0xe8))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Rgb(0xa5, 0xac, 0xb6))
        };
        lines.push(Line::from(Span::styled(text, style)));
    }
    lines.push(Line::from(""));
}

fn build_agent_detail_lines(
    manager: &AgentManagerState,
    idx: usize,
    width: usize,
    content_height: usize,
    lines: &mut Vec<Line<'static>>,
) {
    let Some(record) = manager.records.get(idx) else {
        return;
    };
    lines.push(Line::from(format!(
        "{}  {}",
        record.name,
        record.source_kind.label()
    )));
    lines.push(Line::from(format!("描述: {}", record.description)));
    push_agent_tool_summary(lines, &record.tools, &record.disallow_tools);
    lines.push(Line::from(format!(
        "模型: {}",
        record.model.as_deref().unwrap_or("默认模型")
    )));
    if let Some(path) = &record.path {
        lines.push(Line::from(format!("路径: {}", path.display())));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "系统指令",
        Style::default()
            .fg(Color::Rgb(140, 145, 155))
            .add_modifier(Modifier::BOLD),
    )));
    let actions: &[&str] = if record.editable {
        &["编辑", "删除", "返回"]
    } else {
        &["返回"]
    };
    let action_line_count = actions.len() + 4;
    let instruction_lines = crate::widgets::word_wrap(&record.instructions, width);
    let instruction_limit = if content_height == usize::MAX {
        instruction_lines.len()
    } else {
        content_height
            .saturating_sub(lines.len())
            .saturating_sub(action_line_count)
    };
    for line in instruction_lines.into_iter().take(instruction_limit) {
        lines.push(Line::from(line));
    }
    if content_height != usize::MAX {
        let spacer = content_height.saturating_sub(lines.len() + action_line_count);
        for _ in 0..spacer {
            lines.push(Line::from(""));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "操作",
        Style::default()
            .fg(Color::Rgb(140, 145, 155))
            .add_modifier(Modifier::BOLD),
    )));
    for (action_idx, action) in actions.iter().enumerate() {
        let selected = action_idx == manager.detail_action_selected.min(actions.len() - 1);
        let style = if selected {
            Style::default()
                .fg(Color::Rgb(0x42, 0xd9, 0xe8))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Rgb(0xa5, 0xac, 0xb6))
        };
        lines.push(Line::from(Span::styled(
            format!("{} {}", if selected { "❯" } else { " " }, action),
            style,
        )));
    }
    lines.push(Line::from(""));
}

fn build_agent_edit_menu_lines(manager: &AgentManagerState, lines: &mut Vec<Line<'static>>) {
    lines.push(Line::from(Span::styled(
        format!("编辑 agent: {}", manager.draft.name),
        Style::default()
            .fg(Color::Rgb(0x42, 0xd9, 0xe8))
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("来源: ", Style::default().fg(Color::Rgb(140, 145, 155))),
        Span::raw(manager.draft.source_kind.label()),
    ]));
    if let Some(path) = &manager.draft.original_path {
        lines.push(Line::from(vec![
            Span::styled("路径: ", Style::default().fg(Color::Rgb(140, 145, 155))),
            Span::raw(path.display().to_string()),
        ]));
    }
    lines.push(Line::from(""));

    let actions = ["编辑内容", "编辑工具", "编辑模型", "返回列表"];
    for (idx, action) in actions.iter().enumerate() {
        let selected = idx == manager.edit_action_selected.min(actions.len() - 1);
        lines.push(Line::from(Span::styled(
            format!("{} {}", if selected { "❯" } else { " " }, action),
            selectable_style(selected),
        )));
    }
}

fn build_agent_edit_metadata_lines(
    manager: &AgentManagerState,
    width: usize,
    content_height: usize,
    lines: &mut Vec<Line<'static>>,
) {
    lines.push(Line::from(Span::styled(
        "编辑内容",
        Style::default()
            .fg(Color::Rgb(0x42, 0xd9, 0xe8))
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(vec![
        Span::styled("来源: ", Style::default().fg(Color::Rgb(140, 145, 155))),
        Span::raw(manager.draft.source_kind.label()),
    ]));
    lines.push(Line::from(""));
    editor_field_box(
        lines,
        "名称",
        &manager.draft.name,
        manager.draft.field == AgentEditorField::Name,
        width,
        "未填写",
    );
    let description_cursor = if manager.draft.field == AgentEditorField::Description {
        manager.draft.cursor
    } else {
        manager.draft.description.chars().count()
    };
    editor_text_box(
        lines,
        "描述",
        &manager.draft.description,
        description_cursor,
        manager.draft.field == AgentEditorField::Description,
        EditorTextBoxLayout {
            width,
            max_lines: AGENT_DESCRIPTION_MAX_LINES,
        },
        "未填写",
    );
    let instructions_cursor = if manager.draft.field == AgentEditorField::Instructions {
        manager.draft.cursor
    } else {
        manager.draft.instructions.chars().count()
    };
    editor_text_box(
        lines,
        "系统指令",
        &manager.draft.instructions,
        instructions_cursor,
        manager.draft.field == AgentEditorField::Instructions,
        EditorTextBoxLayout {
            width,
            max_lines: text_box_max_lines(
                lines.len(),
                content_height,
                AGENT_EDIT_CONTENT_INSTRUCTIONS_MAX_LINES,
            ),
        },
        "输入 agent 的系统指令。",
    );
}

fn build_agent_editor_lines(
    manager: &AgentManagerState,
    width: usize,
    content_height: usize,
    lines: &mut Vec<Line<'static>>,
) {
    lines.push(Line::from(format!(
        "{}级 agent  {}",
        manager.draft.source_kind.label(),
        manager
            .draft
            .original_path
            .as_ref()
            .map(|_| "编辑")
            .unwrap_or("创建")
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled("来源  ", Style::default().fg(Color::Rgb(140, 145, 155))),
        Span::raw(manager.draft.source_kind.label()),
    ]));
    push_agent_tool_summary(lines, &manager.draft.tools, &manager.draft.disallow_tools);
    lines.push(Line::from(vec![
        Span::styled("模型  ", Style::default().fg(Color::Rgb(140, 145, 155))),
        Span::raw(format_agent_model(manager.draft.model.as_deref())),
    ]));
    lines.push(Line::from(""));
    editor_field_box(
        lines,
        "名称",
        &manager.draft.name,
        manager.draft.field == AgentEditorField::Name,
        width,
        "未填写",
    );
    let description_cursor = if manager.draft.field == AgentEditorField::Description {
        manager.draft.cursor
    } else {
        manager.draft.description.chars().count()
    };
    editor_text_box(
        lines,
        "描述",
        &manager.draft.description,
        description_cursor,
        manager.draft.field == AgentEditorField::Description,
        EditorTextBoxLayout {
            width,
            max_lines: AGENT_DESCRIPTION_MAX_LINES,
        },
        "未填写",
    );
    let instructions_cursor = if manager.draft.field == AgentEditorField::Instructions {
        manager.draft.cursor
    } else {
        manager.draft.instructions.chars().count()
    };
    editor_text_box(
        lines,
        "系统指令",
        &manager.draft.instructions,
        instructions_cursor,
        manager.draft.field == AgentEditorField::Instructions,
        EditorTextBoxLayout {
            width,
            max_lines: text_box_max_lines(lines.len(), content_height, 10),
        },
        "输入 agent 的系统指令。",
    );

    if manager.draft.original_path.is_some() {
        lines.push(Line::from(""));
        match manager.draft.field {
            AgentEditorField::Model => {
                let remaining_height = content_height.saturating_sub(lines.len());
                build_agent_model_lines(manager, remaining_height, lines);
                lines.push(Line::from(""));
                editor_summary_row(lines, "工具", agent_tool_summary_text(manager), false);
                pad_lines_to_section_height(lines, AGENT_TOOLS_SECTION_LINES, 1);
            }
            AgentEditorField::Tools => {
                let remaining_height = content_height.saturating_sub(lines.len());
                build_agent_tools_lines(manager, remaining_height, lines);
                lines.push(Line::from(""));
                editor_summary_row(
                    lines,
                    "模型",
                    format_agent_model(manager.draft.model.as_deref()),
                    false,
                );
                pad_lines_to_section_height(lines, agent_model_section_line_count(manager), 1);
            }
            _ => {
                editor_summary_row(lines, "工具", agent_tool_summary_text(manager), false);
                pad_lines_to_section_height(lines, AGENT_TOOLS_SECTION_LINES, 1);
                lines.push(Line::from(""));
                editor_summary_row(
                    lines,
                    "模型",
                    format_agent_model(manager.draft.model.as_deref()),
                    false,
                );
                pad_lines_to_section_height(lines, agent_model_section_line_count(manager), 1);
            }
        }
    }
}

fn build_agent_create_lines(
    manager: &AgentManagerState,
    step: AgentCreateStep,
    width: usize,
    content_height: usize,
    lines: &mut Vec<Line<'static>>,
) {
    push_agent_create_tabs(lines, step);
    lines.push(Line::from(""));
    match step {
        AgentCreateStep::Scope => {
            push_agent_section(lines, "保存位置");
            lines.push(Line::from(""));
            lines.push(selectable_row(
                manager.create_scope_selected == 0,
                "",
                "项目级",
                "写入当前项目 .omini/agents",
            ));
            lines.push(selectable_row(
                manager.create_scope_selected == 1,
                "",
                "用户级",
                "写入 ~/.omini/agents",
            ));
            lines.push(Line::from(""));
        }
        AgentCreateStep::Tools => {
            let remaining_height = content_height.saturating_sub(lines.len());
            build_agent_tools_lines(manager, remaining_height, lines);
        }
        AgentCreateStep::Model => {
            let remaining_height = content_height.saturating_sub(lines.len());
            build_agent_model_lines(manager, remaining_height, lines);
        }
        AgentCreateStep::Method => {
            push_agent_section(lines, "创建方式");
            lines.push(Line::from(""));
            lines.push(selectable_row(
                manager.create_method_selected == 0,
                "",
                "LLM 创建",
                "下一步输入用途描述，生成草稿后可预览保存",
            ));
            lines.push(selectable_row(
                manager.create_method_selected == 1,
                "",
                "手动创建",
                "逐步填写名称、描述和系统指令",
            ));
            lines.push(Line::from(""));
        }
        AgentCreateStep::ManualName => {
            manager_summary_lines(manager, lines);
            editor_field_box(
                lines,
                "名称",
                &manager.draft.name,
                manager.draft.field == AgentEditorField::Name,
                width,
                "agent-name",
            );
            lines.push(Line::from(""));
        }
        AgentCreateStep::ManualDescription => {
            manager_summary_lines(manager, lines);
            editor_text_box(
                lines,
                "描述",
                &manager.draft.description,
                manager.draft.cursor,
                manager.draft.field == AgentEditorField::Description,
                EditorTextBoxLayout {
                    width,
                    max_lines: AGENT_DESCRIPTION_MAX_LINES,
                },
                "这个 agent 适合做什么",
            );
            lines.push(Line::from(""));
        }
        AgentCreateStep::ManualInstructions => {
            manager_summary_lines(manager, lines);
            editor_text_box(
                lines,
                "系统指令",
                &manager.draft.instructions,
                manager.draft.cursor,
                true,
                EditorTextBoxLayout {
                    width,
                    max_lines: text_box_max_lines(lines.len(), content_height, 12),
                },
                "输入 agent 的系统指令。",
            );
            lines.push(Line::from(""));
        }
        AgentCreateStep::GenerateDescription => {
            build_agent_generate_lines(manager, width, content_height, true, lines);
        }
    }
}

fn push_agent_create_tabs(lines: &mut Vec<Line<'static>>, step: AgentCreateStep) {
    let tabs = [
        (AgentCreateStep::Scope, "范围"),
        (AgentCreateStep::Tools, "工具"),
        (AgentCreateStep::Model, "模型"),
        (AgentCreateStep::Method, "方式"),
        (step, agent_create_step_label(step)),
    ];
    let mut spans = Vec::new();
    let mut seen = Vec::new();
    for (tab_step, label) in tabs {
        if seen.contains(&tab_step) {
            continue;
        }
        seen.push(tab_step);
        let active = tab_step == step;
        spans.push(Span::styled(
            format!(" {} ", label),
            if active {
                Style::default()
                    .fg(Color::Rgb(0x42, 0xd9, 0xe8))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Rgb(140, 145, 155))
            },
        ));
        spans.push(Span::styled(
            "›",
            Style::default().fg(Color::Rgb(80, 86, 96)),
        ));
    }
    spans.pop();
    lines.push(Line::from(spans));
}

fn agent_create_step_label(step: AgentCreateStep) -> &'static str {
    match step {
        AgentCreateStep::Scope => "范围",
        AgentCreateStep::Tools => "工具",
        AgentCreateStep::Model => "模型",
        AgentCreateStep::Method => "方式",
        AgentCreateStep::ManualName => "名称",
        AgentCreateStep::ManualDescription => "描述",
        AgentCreateStep::ManualInstructions => "指令",
        AgentCreateStep::GenerateDescription => "生成",
    }
}

fn manager_summary_lines(manager: &AgentManagerState, lines: &mut Vec<Line<'static>>) {
    lines.push(Line::from(vec![
        Span::styled("来源  ", Style::default().fg(Color::Rgb(140, 145, 155))),
        Span::raw(manager.draft.source_kind.label()),
    ]));
    push_agent_tool_summary(lines, &manager.draft.tools, &manager.draft.disallow_tools);
    lines.push(Line::from(vec![
        Span::styled("模型  ", Style::default().fg(Color::Rgb(140, 145, 155))),
        Span::raw(format_agent_model(manager.draft.model.as_deref())),
    ]));
    lines.push(Line::from(""));
}

fn build_agent_generate_lines(
    manager: &AgentManagerState,
    width: usize,
    content_height: usize,
    create_flow: bool,
    lines: &mut Vec<Line<'static>>,
) {
    let _ = create_flow;
    lines.push(Line::from(format!(
        "描述生成 {}级 agent",
        manager.draft.source_kind.label()
    )));
    push_agent_tool_summary(lines, &manager.draft.tools, &manager.draft.disallow_tools);
    lines.push(Line::from(vec![
        Span::styled("模型  ", Style::default().fg(Color::Rgb(140, 145, 155))),
        Span::raw(format_agent_model(manager.draft.model.as_deref())),
    ]));
    lines.push(Line::from(""));
    editor_text_box(
        lines,
        "用途描述",
        &manager.draft.generated_description,
        manager.draft.cursor,
        true,
        EditorTextBoxLayout {
            width,
            max_lines: text_box_max_lines(lines.len(), content_height, 12),
        },
        "例如：擅长翻译 Rust 代码注释，只读取必要文件，保持代码不变。",
    );
}

fn text_box_max_lines(lines_before_box: usize, content_height: usize, preferred: usize) -> usize {
    if content_height == usize::MAX {
        return preferred;
    }
    content_height
        .saturating_sub(lines_before_box.saturating_add(3))
        .clamp(1, preferred)
}

fn build_agent_generating_lines(
    manager: &AgentManagerState,
    width: usize,
    lines: &mut Vec<Line<'static>>,
) {
    lines.push(Line::from(vec![
        Span::raw(format!(
            "描述生成 {}级 agent  ",
            manager.draft.source_kind.label()
        )),
        Span::styled(
            "生成中",
            Style::default()
                .fg(Color::Rgb(0x42, 0xd9, 0xe8))
                .add_modifier(Modifier::BOLD),
        ),
    ]));
    lines.push(Line::from(""));
    push_agent_tool_summary(lines, &manager.draft.tools, &manager.draft.disallow_tools);
    lines.push(Line::from(vec![
        Span::styled("模型  ", Style::default().fg(Color::Rgb(140, 145, 155))),
        Span::raw(format_agent_model(manager.draft.model.as_deref())),
    ]));
    lines.push(Line::from(""));
    lines.push(Line::from(status::animated_status_spans(
        "正在生成 agent...",
    )));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "用途描述",
        Style::default()
            .fg(Color::Rgb(140, 145, 155))
            .add_modifier(Modifier::BOLD),
    )));
    let description = manager.draft.generated_description.trim();
    let description = if description.is_empty() {
        "未填写"
    } else {
        description
    };
    for line in crate::widgets::word_wrap(description, width)
        .into_iter()
        .take(8)
    {
        lines.push(Line::from(line));
    }
}

fn build_agent_tools_lines(
    manager: &AgentManagerState,
    max_lines: usize,
    lines: &mut Vec<Line<'static>>,
) {
    lines.extend(scrollable_lines(
        agent_tool_lines(manager),
        max_lines,
        "↑ 更多工具",
        "↓ 更多工具",
    ));
}

fn agent_tool_lines(manager: &AgentManagerState) -> Vec<ScrollableLine> {
    let mut lines = Vec::new();
    lines.push(section_tool_line("工具策略"));
    lines.push(raw_tool_line(""));
    let inherited = manager.draft.tools.is_empty();
    lines.push(indexed_tool_line(
        manager.tool_selected == 0,
        inherited && manager.draft.disallow_tools.is_empty(),
        "继承全部",
        if manager.draft.disallow_tools.is_empty() {
            "使用父会话可用工具"
        } else {
            "使用父会话工具，排除禁用项"
        },
    ));
    lines.push(raw_tool_line(""));
    lines.push(section_tool_line(if inherited {
        "有效工具"
    } else {
        "仅允许"
    }));
    lines.push(raw_tool_line(""));
    let allow_rows = [
        ("读工具组", "search, read", &["search", "read"][..]),
        (
            "写工具组",
            "bash, edit, write",
            &["bash", "edit", "write"][..],
        ),
        ("search", "搜索文件", &["search"][..]),
        ("read", "读取文件", &["read"][..]),
        ("bash", "执行命令", &["bash"][..]),
        ("edit", "编辑文件", &["edit"][..]),
        ("write", "写入文件", &["write"][..]),
        ("ask_user", "询问用户", &["ask_user"][..]),
        ("skill", "加载技能", &["skill"][..]),
    ];
    for (offset, (label, desc, tools)) in allow_rows.iter().enumerate() {
        let idx = offset + 1;
        let enabled = tools
            .iter()
            .all(|tool| tool_effectively_enabled(manager, tool));
        lines.push(indexed_tool_line(
            idx == manager.tool_selected,
            enabled,
            label,
            desc,
        ));
    }
    lines.push(raw_tool_line(""));
    lines.push(section_tool_line("禁用"));
    lines.push(raw_tool_line(""));
    let deny_rows = [
        ("search", "不搜索文件"),
        ("read", "不读取文件"),
        ("bash", "不执行命令"),
        ("edit", "不编辑文件"),
        ("write", "不写入文件"),
        ("ask_user", "不询问用户"),
        ("skill", "不加载技能"),
    ];
    for (offset, (label, desc)) in deny_rows.iter().enumerate() {
        let idx = offset + 10;
        let enabled = manager
            .draft
            .disallow_tools
            .iter()
            .any(|tool| tool == label);
        lines.push(indexed_tool_line(
            idx == manager.tool_selected,
            enabled,
            label,
            desc,
        ));
    }
    lines
}

fn raw_tool_line(text: &str) -> ScrollableLine {
    ScrollableLine {
        selected: false,
        line: Line::from(text.to_string()),
    }
}

fn section_tool_line(label: &str) -> ScrollableLine {
    ScrollableLine {
        selected: false,
        line: Line::from(Span::styled(
            label.to_string(),
            Style::default()
                .fg(Color::Rgb(140, 145, 155))
                .add_modifier(Modifier::BOLD),
        )),
    }
}

fn indexed_tool_line(selected: bool, enabled: bool, label: &str, desc: &str) -> ScrollableLine {
    ScrollableLine {
        selected,
        line: tool_row(selected, enabled, label, desc),
    }
}

fn tool_effectively_enabled(manager: &AgentManagerState, tool: &str) -> bool {
    if manager.draft.disallow_tools.iter().any(|item| item == tool) {
        return false;
    }
    manager.draft.tools.is_empty() || manager.draft.tools.iter().any(|item| item == tool)
}

fn tool_row(selected: bool, enabled: bool, label: &str, desc: &str) -> Line<'static> {
    Line::from(format!(
        "{} [{}] {:<10} {}",
        if selected { "❯" } else { " " },
        if enabled { "x" } else { " " },
        label,
        desc
    ))
}

fn push_agent_tool_summary(
    lines: &mut Vec<Line<'static>>,
    tools: &[String],
    disallow_tools: &[String],
) {
    if tools.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("工具  ", Style::default().fg(Color::Rgb(140, 145, 155))),
            Span::raw(if disallow_tools.is_empty() {
                "继承全部".to_string()
            } else {
                format!("继承全部，禁用 {}", format_tool_names(disallow_tools))
            }),
        ]));
    } else {
        lines.push(Line::from(vec![
            Span::styled("工具  ", Style::default().fg(Color::Rgb(140, 145, 155))),
            Span::raw(format!("仅允许 {}", format_tool_names(tools))),
        ]));
        if !disallow_tools.is_empty() {
            lines.push(Line::from(vec![
                Span::styled("禁用  ", Style::default().fg(Color::Rgb(140, 145, 155))),
                Span::raw(format_tool_names(disallow_tools)),
            ]));
        }
    }
}

fn agent_tool_summary_line_count(tools: &[String], disallow_tools: &[String]) -> usize {
    if tools.is_empty() || disallow_tools.is_empty() {
        1
    } else {
        2
    }
}

fn build_agent_model_lines(
    manager: &AgentManagerState,
    max_lines: usize,
    lines: &mut Vec<Line<'static>>,
) {
    lines.extend(scrollable_lines(
        agent_model_lines(manager),
        max_lines,
        "↑ 更多模型",
        "↓ 更多模型",
    ));
}

fn agent_model_lines(manager: &AgentManagerState) -> Vec<ScrollableLine> {
    let mut lines = Vec::new();
    lines.push(scroll_section_line("当前配置"));
    lines.push(unselected_line(Line::from("")));
    for (idx, entry) in manager.model_entries.iter().enumerate() {
        match entry {
            AgentModelEntry::Inherit => {
                let selected = idx == manager.model_selected;
                lines.push(ScrollableLine {
                    selected,
                    line: Line::from(Span::styled(
                        format!(
                            "{} 继承主会话模型{}",
                            if selected { "❯" } else { " " },
                            if manager.draft.model.is_none() {
                                " ✔"
                            } else {
                                ""
                            }
                        ),
                        Style::default().fg(Color::Rgb(0xa5, 0xac, 0xb6)),
                    )),
                });
                lines.push(unselected_line(Line::from("")));
                lines.push(scroll_section_line("可选模型"));
                lines.push(unselected_line(Line::from("")));
            }
            AgentModelEntry::ProviderHeader { name } => {
                lines.push(ScrollableLine {
                    selected: false,
                    line: Line::from(Span::styled(
                        name.clone(),
                        Style::default()
                            .fg(Color::Rgb(140, 145, 155))
                            .add_modifier(Modifier::BOLD),
                    )),
                });
            }
            AgentModelEntry::Model {
                provider_key,
                model,
            } => {
                let selected = idx == manager.model_selected;
                let value = format!("{}/{}", provider_key, model.id);
                lines.push(ScrollableLine {
                    selected,
                    line: Line::from(Span::styled(
                        format!(
                            "{} {}{}",
                            if selected { "❯" } else { " " },
                            model.name.as_deref().unwrap_or(&model.id),
                            if Some(&value) == manager.draft.model.as_ref() {
                                " ✔"
                            } else {
                                ""
                            }
                        ),
                        Style::default().fg(Color::Rgb(0xa5, 0xac, 0xb6)),
                    )),
                });
            }
        }
    }
    lines
}

fn unselected_line(line: Line<'static>) -> ScrollableLine {
    ScrollableLine {
        selected: false,
        line,
    }
}

fn scroll_section_line(label: &str) -> ScrollableLine {
    ScrollableLine {
        selected: false,
        line: Line::from(Span::styled(
            label.to_string(),
            Style::default()
                .fg(Color::Rgb(140, 145, 155))
                .add_modifier(Modifier::BOLD),
        )),
    }
}

fn agent_model_section_line_count(manager: &AgentManagerState) -> usize {
    manager
        .model_entries
        .iter()
        .take(20)
        .map(|entry| match entry {
            AgentModelEntry::Inherit => 4,
            AgentModelEntry::ProviderHeader { .. } | AgentModelEntry::Model { .. } => 1,
        })
        .sum()
}

fn pad_lines_to_section_height(
    lines: &mut Vec<Line<'static>>,
    expected_height: usize,
    current_height: usize,
) {
    for _ in current_height..expected_height {
        lines.push(Line::from(""));
    }
}

fn push_agent_section(lines: &mut Vec<Line<'static>>, label: &str) {
    lines.push(Line::from(Span::styled(
        label.to_string(),
        Style::default()
            .fg(Color::Rgb(140, 145, 155))
            .add_modifier(Modifier::BOLD),
    )));
}

fn selectable_row(selected: bool, key: &str, label: &str, desc: &str) -> Line<'static> {
    let style = selectable_style(selected);
    let key_part = if key.is_empty() {
        String::new()
    } else {
        format!("{key}  ")
    };
    let label_part = pad_display_width(&format!("{key_part}{label}"), 10);
    Line::from(Span::styled(
        format!(
            "{} {} {}",
            if selected { "❯" } else { " " },
            label_part,
            desc
        ),
        style,
    ))
}

fn selectable_style(selected: bool) -> Style {
    if selected {
        Style::default()
            .fg(Color::Rgb(0x42, 0xd9, 0xe8))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Rgb(0xa5, 0xac, 0xb6))
    }
}

fn format_tool_names(tools: &[String]) -> String {
    if tools.is_empty() {
        "无".to_string()
    } else {
        tools.join(", ")
    }
}

fn format_agent_model(model: Option<&str>) -> String {
    model.unwrap_or("继承主会话模型").to_string()
}

fn agent_tool_summary_text(manager: &AgentManagerState) -> String {
    if manager.draft.tools.is_empty() {
        if manager.draft.disallow_tools.is_empty() {
            "继承全部".to_string()
        } else {
            format!(
                "继承全部，禁用 {}",
                format_tool_names(&manager.draft.disallow_tools)
            )
        }
    } else {
        format!("仅允许 {}", format_tool_names(&manager.draft.tools))
    }
}

fn editor_field_box(
    lines: &mut Vec<Line<'static>>,
    label: &str,
    value: &str,
    selected: bool,
    width: usize,
    placeholder: &str,
) {
    push_input_label(lines, label, selected);
    let box_width = input_box_width(width);
    lines.push(box_border_line(box_width, true));
    let (text, style) = if value.is_empty() {
        (placeholder.to_string(), placeholder_style())
    } else {
        (value.to_string(), Style::default())
    };
    lines.push(box_content_line(&text, style, box_width));
    lines.push(box_border_line(box_width, false));
}

struct EditorTextBoxLayout {
    width: usize,
    max_lines: usize,
}

fn editor_text_box(
    lines: &mut Vec<Line<'static>>,
    label: &str,
    value: &str,
    cursor: usize,
    selected: bool,
    layout: EditorTextBoxLayout,
    placeholder: &str,
) {
    push_input_label(lines, label, selected);
    let box_width = input_box_width(layout.width);
    lines.push(box_border_line(box_width, true));
    let wrapped = editable_text_window(
        value,
        cursor,
        input_box_inner_width(layout.width),
        layout.max_lines,
        placeholder,
    );
    let style = if value.is_empty() {
        placeholder_style()
    } else {
        Style::default()
    };
    for line in wrapped.lines {
        lines.push(box_content_line(&line, style, box_width));
    }
    lines.push(box_border_line(box_width, false));
}

fn push_input_label(lines: &mut Vec<Line<'static>>, label: &str, selected: bool) {
    let style = if selected {
        Style::default()
            .fg(Color::Rgb(0x42, 0xd9, 0xe8))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Rgb(140, 145, 155))
    };
    lines.push(Line::from(Span::styled(label.to_string(), style)));
}

fn editor_summary_row(lines: &mut Vec<Line<'static>>, label: &str, value: String, selected: bool) {
    let label_style = if selected {
        Style::default()
            .fg(Color::Rgb(0x42, 0xd9, 0xe8))
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Rgb(140, 145, 155))
    };
    lines.push(Line::from(vec![
        Span::styled(format!("{label}  "), label_style),
        Span::raw(value),
    ]));
}

fn input_box_width(width: usize) -> usize {
    width.clamp(4, AGENT_EDITOR_MAX_WIDTH)
}

fn box_border_line(width: usize, top: bool) -> Line<'static> {
    let (left, right) = if top { ("┌", "┐") } else { ("└", "┘") };
    Line::from(Span::styled(
        format!("{left}{}{right}", "─".repeat(width.saturating_sub(2))),
        Style::default().fg(Color::Rgb(80, 86, 96)),
    ))
}

fn box_content_line(text: &str, text_style: Style, width: usize) -> Line<'static> {
    let inner_width = width.saturating_sub(4);
    let display = truncate_str(text, inner_width);
    let text_width = display.width();
    let padding = inner_width.saturating_sub(text_width);
    Line::from(vec![
        Span::styled("│ ", Style::default().fg(Color::Rgb(80, 86, 96))),
        Span::styled(display, text_style),
        Span::raw(" ".repeat(padding)),
        Span::styled(" │", Style::default().fg(Color::Rgb(80, 86, 96))),
    ])
}

fn placeholder_style() -> Style {
    Style::default().fg(Color::Rgb(100, 106, 116))
}

fn hint(text: &str) -> Line<'static> {
    Line::from(Span::styled(
        text.to_string(),
        Style::default().fg(Color::Rgb(140, 145, 155)),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subagents::{AgentDraft, AgentRecord, AgentSourceKind};
    use crate::types::config::ModelConfig;
    use std::collections::HashMap;

    fn line_text(line: &Line<'static>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    fn manager_with_tool_selected(tool_selected: usize) -> AgentManagerState {
        let mut manager =
            AgentManagerState::new(Vec::new(), HashMap::new(), String::new(), String::new());
        manager.draft.source_kind = AgentSourceKind::Project;
        manager.tool_selected = tool_selected;
        manager
    }

    fn manager_with_model_selected(model_selected: usize) -> AgentManagerState {
        let mut manager =
            AgentManagerState::new(Vec::new(), HashMap::new(), String::new(), String::new());
        manager.model_entries = vec![AgentModelEntry::Inherit];
        for idx in 0..6 {
            manager.model_entries.push(AgentModelEntry::ProviderHeader {
                name: format!("Provider {idx}"),
            });
            manager.model_entries.push(AgentModelEntry::Model {
                provider_key: format!("provider-{idx}"),
                model: ModelConfig {
                    id: format!("model-{idx}"),
                    name: Some(format!("Model {idx}")),
                    limit: 1000,
                    thinking: false,
                },
            });
        }
        manager.model_selected = model_selected;
        manager
    }

    fn numbered_lines(prefix: &str, count: usize) -> String {
        (0..count)
            .map(|idx| format!("{prefix} {idx:02}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn selectable_row_aligns_descriptions_by_display_width() {
        let llm = line_text(&selectable_row(true, "", "LLM 创建", "下一步输入用途描述"));
        let manual = line_text(&selectable_row(false, "", "手动创建", "逐步填写名称"));

        let llm_prefix = llm
            .split_once("下一步输入用途描述")
            .expect("description should be present")
            .0;
        let manual_prefix = manual
            .split_once("逐步填写名称")
            .expect("description should be present")
            .0;

        assert_eq!(llm_prefix.width(), manual_prefix.width());
    }

    #[test]
    fn tool_lines_keep_bottom_selected_row_visible_when_cropped() {
        let manager = manager_with_tool_selected(16);
        let mut lines = Vec::new();

        build_agent_tools_lines(&manager, 5, &mut lines);
        let rendered = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");

        assert!(rendered.contains("↑ 更多工具"));
        assert!(rendered.contains("❯ [ ] skill"));
        assert!(rendered.contains("不加载技能"));
        assert!(!rendered.contains("↓ 更多工具"));
    }

    #[test]
    fn tool_lines_show_both_scroll_indicators_for_middle_window() {
        let manager = manager_with_tool_selected(8);
        let mut lines = Vec::new();

        build_agent_tools_lines(&manager, 5, &mut lines);
        let rendered = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");

        assert!(rendered.contains("↑ 更多工具"));
        assert!(rendered.contains("↓ 更多工具"));
        assert!(rendered.contains("❯ [x] ask_user"));
    }

    #[test]
    fn tool_lines_do_not_show_scroll_indicators_when_full_height_fits() {
        let manager = manager_with_tool_selected(16);
        let mut lines = Vec::new();

        build_agent_tools_lines(&manager, usize::MAX, &mut lines);
        let rendered = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");

        assert!(!rendered.contains("更多工具"));
        assert!(rendered.contains("❯ [ ] skill"));
    }

    #[test]
    fn model_lines_keep_bottom_selected_row_visible_when_cropped() {
        let manager = manager_with_model_selected(12);
        let mut lines = Vec::new();

        build_agent_model_lines(&manager, 5, &mut lines);
        let rendered = lines.iter().map(line_text).collect::<Vec<_>>().join("\n");

        assert!(rendered.contains("↑ 更多模型"));
        assert!(rendered.contains("❯ Model 5"));
        assert!(!rendered.contains("↓ 更多模型"));
    }

    #[test]
    fn selected_agent_model_uses_arrow_without_text_highlight() {
        let manager = manager_with_model_selected(0);
        let lines = agent_model_lines(&manager);
        let selected = lines
            .into_iter()
            .find(|line| line.selected)
            .expect("selected model line should exist");

        assert!(line_text(&selected.line).contains("❯ 继承主会话模型"));
        assert_ne!(
            selected.line.spans[0].style.fg,
            Some(Color::Rgb(0x42, 0xd9, 0xe8))
        );
    }

    #[test]
    fn generated_preview_cursor_uses_cropped_instruction_box_height() {
        let width = 80;
        let content_height = 19;
        let instructions = numbered_lines("step", 20);
        let mut manager =
            AgentManagerState::new(Vec::new(), HashMap::new(), String::new(), String::new());
        manager.apply_generated(
            AgentSourceKind::Project,
            AgentDraft {
                name: "code-review".to_string(),
                description: "Reviews code changes.".to_string(),
                instructions,
                tools: Vec::new(),
                disallow_tools: Vec::new(),
                model: None,
            },
        );
        manager.draft.field = AgentEditorField::Instructions;
        manager.move_draft_cursor_to_current_end();

        let lines = build_agents_lines(&manager, width, content_height);
        let (cursor_line, _) =
            agent_editor_cursor(&manager, width, content_height).expect("cursor should render");

        assert!(cursor_line < content_height);
        assert_eq!(cursor_line, content_height - 1);
        assert!(line_text(&lines[cursor_line]).starts_with("│ "));
        assert!(line_text(&lines[cursor_line]).contains("step 19"));
    }

    #[test]
    fn edit_metadata_cursor_uses_cropped_instruction_box_height() {
        let width = 80;
        let content_height = 16;
        let instructions = numbered_lines("rule", 20);
        let mut manager =
            AgentManagerState::new(Vec::new(), HashMap::new(), String::new(), String::new());
        manager.start_edit(AgentRecord {
            name: "code-review".to_string(),
            description: "Reviews code changes.".to_string(),
            instructions,
            tools: Vec::new(),
            disallow_tools: Vec::new(),
            model: None,
            source_kind: AgentSourceKind::Project,
            path: None,
            editable: true,
        });
        manager.view = AgentManagerView::EditMetadata;
        manager.draft.field = AgentEditorField::Instructions;
        manager.move_draft_cursor_to_current_end();

        let lines = build_agents_lines(&manager, width, content_height);
        let (cursor_line, _) =
            agent_editor_cursor(&manager, width, content_height).expect("cursor should render");

        assert!(cursor_line < content_height);
        assert_eq!(cursor_line, content_height - 1);
        assert!(line_text(&lines[cursor_line]).starts_with("│ "));
        assert!(line_text(&lines[cursor_line]).contains("rule 19"));
    }

    #[test]
    fn generate_cursor_uses_same_height_as_rendered_text_box() {
        let width = 80;
        let content_height = 10;
        let mut manager =
            AgentManagerState::new(Vec::new(), HashMap::new(), String::new(), String::new());
        manager.view = AgentManagerView::Generate;
        manager.draft.field = AgentEditorField::GenerateDescription;
        manager.draft.generated_description = numbered_lines("goal", 20);
        manager.move_draft_cursor_to_current_end();

        let lines = build_agents_lines(&manager, width, content_height);
        let (cursor_line, _) =
            agent_editor_cursor(&manager, width, content_height).expect("cursor should render");

        assert!(cursor_line < content_height);
        assert!(line_text(&lines[cursor_line]).starts_with("│ "));
        assert!(line_text(&lines[cursor_line]).contains("goal 19"));
    }
}
