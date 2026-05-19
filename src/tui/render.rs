use crate::tui::selection::{highlighted_line, selected_cols_for_screen_line};
use crate::tui::state::{
    AgentCreateStep, AgentEditorField, AgentManagerState, AgentManagerView, AgentModelEntry,
    InteractionStep, ModelSelectionEntry, SubagentNode, UiMessage, UiState,
};
use crate::tui::widgets::{
    build_bordered_lines, build_plain_lines, build_thinking_lines, display_path, render_tool,
};
use crate::types::display::{DisplayMention, DisplayMessage, MentionKind};
use crate::types::events::{PermissionPreview, SubagentStatus, ToolPauseKind, ToolPauseRequest};
use crate::types::message::{ContentBlock, TextBlock, ToolResultBlock, ToolUseBlock};
use chrono::DateTime;
use chrono::Local;
use chrono::Utc;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Clear, Paragraph};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

mod autocomplete;
mod input;
mod status;

const PERMISSION_DRAWER_MAX_HEIGHT: u16 = 18;
const EDIT_PERMISSION_DRAWER_MAX_HEIGHT: u16 = 50;
const AGENT_EDITOR_MAX_WIDTH: usize = 140;
const AGENT_TOOLS_SECTION_LINES: usize = 21;
const AGENT_EDIT_CONTENT_INSTRUCTIONS_MAX_LINES: usize = 10;
const USER_INPUT_NONE_LABEL: &str = "None of the above";
const USER_INPUT_NONE_DESCRIPTION: &str = "Optionally, add details in notes (tab).";
const USER_INPUT_NOTE_PREFIX: &str = "› ";
const USER_INPUT_NOTE_PLACEHOLDER: &str = "Add notes";

struct DrawerLines {
    lines: Vec<Line<'static>>,
    note_line_index: Option<usize>,
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

pub fn render(state: &mut UiState, frame: &mut ratatui::Frame) {
    let area = frame.area();
    state.clear_selectable_screen_lines();
    // 整体背景色 #282c34
    frame.render_widget(
        Paragraph::new(Line::from("")).style(Style::default().bg(Color::Rgb(40, 44, 52))),
        area,
    );

    // 会话列表：全屏模式（替换整个界面）
    if let Some(InteractionStep::Session { .. }) = &state.interaction_step {
        render_session_list(state, frame, area);
        return;
    }

    let drawer_len = input::queued_drawer_inputs(state).len();
    let queued_height = if drawer_len == 0 {
        0
    } else {
        drawer_len.min(4) as u16 + 2
    };
    state.set_input_wrap_width(area.width as usize);
    let input_height = 2 + state.input_visible_line_count() as u16 + queued_height;
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(input_height),
        Constraint::Length(1),
    ])
    .split(area);
    state.messages_area = chunks[1];

    render_messages(state, frame, chunks[1]);
    autocomplete::render_autocomplete(state, frame, chunks[3]);
    status::render_footer(state, frame, chunks[4]);

    // Draw input box only when no modal interaction is active (prevents cursor showing through overlay)
    if state.interaction_step.is_none() && state.active_tool_pause().is_none() {
        input::render_input(state, frame, chunks[3]);
    }

    // 模型选择等弹窗：覆盖在正常布局之上（不遮盖消息区背景）
    if state.interaction_request.is_some() {
        render_interaction(state, frame, area);
    }

    render_permission_drawer(state, frame, area);
}

fn render_permission_drawer(state: &mut UiState, frame: &mut ratatui::Frame, area: Rect) {
    let Some(request) = state.active_tool_pause().cloned() else {
        state.permission_drawer_area = Rect::default();
        state.permission_drawer_body_area = Rect::default();
        state.permission_drawer_content_len = 0;
        return;
    };

    let project_dir = state.status_bar.cwd.clone();
    let preview_tool_use_id = request
        .preview_tool_use_id
        .as_deref()
        .unwrap_or(&request.tool_use_id);
    let tool_use = find_tool_use(state, preview_tool_use_id);
    let content_width = area.width.saturating_sub(6) as usize;
    let DrawerLines {
        lines,
        note_line_index,
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
    let is_edit_preview = matches!(
        request.kind,
        ToolPauseKind::Permission(PermissionPreview::Edit(_))
            | ToolPauseKind::Permission(PermissionPreview::Write(_))
    );

    let terminal_cap = ((area.height as f32) * 0.8).floor() as u16;
    let max_height = if is_edit_preview {
        terminal_cap
            .min(EDIT_PERMISSION_DRAWER_MAX_HEIGHT)
            .min(area.height.saturating_sub(1))
            .max(7)
    } else {
        area.height
            .saturating_sub(4)
            .clamp(7, PERMISSION_DRAWER_MAX_HEIGHT)
    };
    let desired_height = (scroll_lines.len() as u16)
        .saturating_add(8)
        .clamp(10, max_height);
    let body_height = desired_height.saturating_sub(7) as usize;
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
            " Question {}/{} ({} unanswered) ",
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

    if let Some(header) = fixed_header {
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

    let paragraph = Paragraph::new(Text::from(visible_lines));
    frame.render_widget(paragraph, body_area);
    if max_scroll > 0 {
        render_permission_scrollbar(frame, body_area, scroll_y, scroll_line_count);
    }

    if state.user_input_note_mode
        && let (Some(note_line_idx), Some(note_cursor_column)) =
            (note_line_index, note_cursor_column)
        && note_line_idx >= scroll_y
        && note_line_idx < scroll_y + body_height
    {
        let cursor_x = body_area.x + note_cursor_column as u16;
        let cursor_y = body_area.y + (note_line_idx - scroll_y) as u16;
        frame.set_cursor_position((cursor_x, cursor_y));
    }

    let options = match &request.kind {
        ToolPauseKind::Permission(_) => build_permission_action_lines(state, &request),
        ToolPauseKind::UserInput(preview) => build_user_input_action_lines(state, preview),
    };
    frame.render_widget(
        Paragraph::new(options),
        Rect {
            x: drawer_area.x + 3,
            y: drawer_area.y + drawer_area.height.saturating_sub(3),
            width: drawer_area.width.saturating_sub(6),
            height: 2,
        },
    );
}

fn build_permission_action_lines(state: &UiState, request: &ToolPauseRequest) -> Text<'static> {
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
            Span::styled(no_desc, desc_style),
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
                "tab or esc ",
                Style::default().fg(Color::Rgb(140, 145, 155)),
            ),
            Span::styled(
                "to finish notes",
                Style::default().fg(Color::Rgb(140, 145, 155)),
            ),
            Span::raw(" | "),
            Span::styled("enter ", Style::default().fg(Color::Rgb(140, 145, 155))),
            Span::styled(
                "to submit answer",
                Style::default().fg(Color::Rgb(140, 145, 155)),
            ),
        ])])
    } else {
        Text::from(vec![Line::from(vec![
            Span::styled(
                "tab to add notes",
                Style::default()
                    .fg(Color::Rgb(0x42, 0xd9, 0xe8))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" | "),
            Span::styled(
                "enter to submit answer",
                Style::default().fg(Color::Rgb(140, 145, 155)),
            ),
            Span::raw(" | "),
            Span::styled(
                "←/→ to navigate questions",
                Style::default().fg(Color::Rgb(140, 145, 155)),
            ),
            Span::raw(" | "),
            Span::styled(
                "esc to interrupt",
                Style::default().fg(Color::Rgb(140, 145, 155)),
            ),
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
        ToolPauseKind::Permission(PermissionPreview::Custom { .. }) => ("allow tool", "deny tool"),
        ToolPauseKind::UserInput(_) => ("submit response", "cancel request"),
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
                    Span::styled(
                        "Description: ",
                        Style::default().fg(Color::Rgb(140, 145, 155)),
                    ),
                    Span::styled(
                        description.trim().to_string(),
                        Style::default().fg(Color::Rgb(220, 220, 225)),
                    ),
                ]));
                lines.push(Line::from(""));
            }
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    "$ ",
                    Style::default()
                        .fg(Color::Rgb(0x50, 0xc8, 0x78))
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    preview.command.clone(),
                    Style::default().fg(Color::Rgb(220, 220, 225)),
                ),
            ]));
            DrawerLines {
                lines,
                note_line_index: None,
                note_cursor_column: None,
            }
        }
        ToolPauseKind::Permission(PermissionPreview::Edit(_)) => {
            let lines = if let Some(tool_use) = tool_use {
                render_tool(tool_use, None, Some(request), content_width, project_dir)
            } else {
                vec![Line::from(Span::styled(
                    "Missing edit tool input for preview",
                    Style::default().fg(Color::Rgb(255, 100, 100)),
                ))]
            };
            DrawerLines {
                lines,
                note_line_index: None,
                note_cursor_column: None,
            }
        }
        ToolPauseKind::Permission(PermissionPreview::Write(_)) => {
            let lines = if let Some(tool_use) = tool_use {
                render_tool(tool_use, None, Some(request), content_width, project_dir)
            } else {
                vec![Line::from(Span::styled(
                    "Missing write tool input for preview",
                    Style::default().fg(Color::Rgb(255, 100, 100)),
                ))]
            };
            DrawerLines {
                lines,
                note_line_index: None,
                note_cursor_column: None,
            }
        }
        ToolPauseKind::Permission(PermissionPreview::Read(preview)) => {
            let lines = if let Some(tool_use) = tool_use {
                render_tool(tool_use, None, Some(request), content_width, project_dir)
            } else {
                vec![Line::from(vec![
                    Span::raw("  "),
                    Span::styled("Path: ", Style::default().fg(Color::Rgb(140, 145, 155))),
                    Span::styled(
                        display_path(&preview.file_path, project_dir),
                        Style::default().fg(Color::Rgb(220, 220, 225)),
                    ),
                ])]
            };
            DrawerLines {
                lines,
                note_line_index: None,
                note_cursor_column: None,
            }
        }
        ToolPauseKind::Permission(_preview) => DrawerLines {
            lines: vec![Line::from("")],
            note_line_index: None,
            note_cursor_column: None,
        },
        ToolPauseKind::UserInput(preview) => {
            let Some(question) = preview.questions.get(question_index) else {
                return DrawerLines {
                    lines: vec![Line::from("Missing question")],
                    note_line_index: None,
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
            let mut note_line_index = None;
            let mut note_cursor_column = None;
            if user_input_note_mode || !current_user_input_note.is_empty() {
                lines.push(Line::from(""));
                note_line_index = Some(lines.len().saturating_sub(1));
                note_cursor_column = Some(user_input_note_cursor_column(
                    current_user_input_note,
                    user_input_note_cursor,
                ));
                lines.push(user_input_note_line(
                    current_user_input_note,
                    user_input_note_mode,
                ));
            }
            DrawerLines {
                lines,
                note_line_index,
                note_cursor_column,
            }
        }
    };
    add_permission_source_line(&mut drawer, request);
    drawer
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
                Span::styled("From: ", Style::default().fg(Color::Rgb(140, 145, 155))),
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
                Span::styled("Rule: ", Style::default().fg(Color::Rgb(140, 145, 155))),
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
    if let Some(index) = drawer.note_line_index.as_mut() {
        *index += insert_at;
    }
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
        crate::tui::widgets::word_wrap(&question.question, content_width)
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

// ===========================================================================
// 交互选择页
// ===========================================================================

fn render_interaction(state: &mut UiState, frame: &mut ratatui::Frame, area: Rect) {
    if let Some(InteractionStep::Agents(manager)) = &mut state.interaction_step {
        manager.set_draft_wrap_width(input_box_inner_width(area.width.saturating_sub(4) as usize));
    }

    if let Some(InteractionStep::Agents(manager)) = state.interaction_step.clone() {
        render_agents_panel(state, frame, area, &manager);
        return;
    }

    // ThinkingEffort is now inlined inside ModelSelection
    let Some(InteractionStep::ModelSelection {
        entries,
        selected,
        thinking_idx,
        active_provider,
        active_model,
    }) = state.interaction_step.clone()
    else {
        return;
    };

    // Panel height
    let has_thinking = entries
        .get(selected)
        .is_some_and(|e| matches!(e, ModelSelectionEntry::Model { model, .. } if model.thinking));
    // title(1) + subtitle(1) + divider(1) + entries + gap(0-1) + thinking(0-1) + hint(1)
    let extra: u16 = if has_thinking { 6 } else { 4 };
    let panel_height = ((entries.len() as u16) + extra)
        .clamp(5, 22)
        .min(area.height.saturating_sub(4).max(1));

    let panel_area = Rect {
        x: area.x,
        y: area.y + area.height - panel_height,
        width: area.width,
        height: panel_height,
    };

    // Clear only — no background color
    frame.render_widget(Clear, panel_area);

    // ── Header: title + subtitle + thick divider ──
    let accent = Color::Rgb(0x42, 0xd9, 0xe8);

    // Line 0: thick divider above the panel (━ characters, accent color)
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

    // Line 1: "Select model" in accent color, bold
    let mut title_line = Line::from(Span::styled(
        " Select model",
        Style::default().fg(accent).add_modifier(Modifier::BOLD),
    ));
    let title_area = Rect {
        x: panel_area.x,
        y: panel_area.y + 1,
        width: panel_area.width,
        height: 1,
    };
    register_and_highlight_lines(state, title_area, std::slice::from_mut(&mut title_line));

    frame.render_widget(Paragraph::new(title_line), title_area);

    // Line 2: Chinese subtitle in gray
    let mut subtitle_line = Line::from(Span::styled(
        " 切换模型，适用于当前会话和未来会话。",
        Style::default().fg(Color::Rgb(140, 145, 155)),
    ));
    let subtitle_area = Rect {
        x: panel_area.x,
        y: panel_area.y + 2,
        width: panel_area.width,
        height: 1,
    };
    register_and_highlight_lines(
        state,
        subtitle_area,
        std::slice::from_mut(&mut subtitle_line),
    );

    frame.render_widget(Paragraph::new(subtitle_line), subtitle_area);

    // Content area below divider
    let content_area = Rect {
        x: panel_area.x,
        y: panel_area.y + 3,
        width: panel_area.width,
        height: panel_area.height - 3,
    };

    render_model_panel(
        state,
        frame,
        content_area,
        ModelPanelParams {
            entries: &entries,
            selected,
            thinking_idx,
            active_provider: &active_provider,
            active_model: &active_model,
        },
    );
}

fn render_agents_panel(
    state: &mut UiState,
    frame: &mut ratatui::Frame,
    area: Rect,
    manager: &AgentManagerState,
) {
    let max_panel_height = ((area.height as f32) * 0.75).round() as u16;
    let max_panel_height = max_panel_height
        .max(28)
        .min(area.height.saturating_sub(4).max(1));
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
    register_and_highlight_lines(state, content_area, &mut rendered);
    frame.render_widget(Paragraph::new(Text::from(rendered)), content_area);

    let mut footer = agents_footer_hint(manager);
    register_and_highlight_lines(state, footer_area, std::slice::from_mut(&mut footer));
    frame.render_widget(Paragraph::new(footer), footer_area);

    if let Some((line_idx, col)) = agent_editor_cursor(
        manager,
        content_area.width as usize,
        content_area.height as usize,
    ) {
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
            build_agent_edit_metadata_lines(manager, width, &mut lines)
        }
        AgentManagerView::EditTools => build_agent_tools_lines(manager, &mut lines),
        AgentManagerView::EditModel => build_agent_model_lines(manager, &mut lines),
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
                let window = editable_text_window(
                    &manager.draft.instructions,
                    manager.draft.cursor,
                    input_width,
                    AGENT_EDIT_CONTENT_INSTRUCTIONS_MAX_LINES,
                    "输入 agent 的系统指令。",
                );
                Some((
                    PANEL_PREFIX_LINES
                        + agent_editor_instructions_cursor_prefix_lines(manager, input_width)
                        + window.cursor_line,
                    2 + window.cursor_col,
                ))
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
                let window = editable_text_window(
                    &manager.draft.instructions,
                    manager.draft.cursor,
                    input_width,
                    AGENT_EDIT_CONTENT_INSTRUCTIONS_MAX_LINES,
                    "输入 agent 的系统指令。",
                );
                Some((
                    PANEL_PREFIX_LINES + 12 + description_lines + window.cursor_line,
                    2 + window.cursor_col,
                ))
            }
            _ => None,
        },
        AgentManagerView::Generate => {
            let window = editable_text_window(
                &manager.draft.generated_description,
                manager.draft.cursor,
                input_width,
                agent_generate_text_max_lines(content_height, false),
                "例如：擅长翻译 Rust 代码注释，只读取必要文件，保持代码不变。",
            );
            Some((
                PANEL_PREFIX_LINES
                    + agent_generate_cursor_prefix_lines(manager, false)
                    + window.cursor_line,
                2 + window.cursor_col,
            ))
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
                let window = editable_text_window(
                    &manager.draft.instructions,
                    manager.draft.cursor,
                    input_width,
                    agent_generate_text_max_lines(content_height, true),
                    "输入 agent 的系统指令。",
                );
                Some((
                    PANEL_PREFIX_LINES
                        + agent_manual_create_field_cursor_prefix_lines(manager)
                        + window.cursor_line,
                    2 + window.cursor_col,
                ))
            }
            AgentCreateStep::GenerateDescription => {
                let window = editable_text_window(
                    &manager.draft.generated_description,
                    manager.draft.cursor,
                    input_width,
                    agent_generate_text_max_lines(content_height, true),
                    "例如：擅长翻译 Rust 代码注释，只读取必要文件，保持代码不变。",
                );
                Some((
                    PANEL_PREFIX_LINES
                        + agent_generate_cursor_prefix_lines(manager, true)
                        + window.cursor_line,
                    2 + window.cursor_col,
                ))
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
    14 + summary_lines + description_lines
}

fn agent_generate_text_max_lines(content_height: usize, create_flow: bool) -> usize {
    if content_height == usize::MAX {
        return 12;
    }
    let reserved = if create_flow { 10 } else { 8 };
    content_height.saturating_sub(reserved).clamp(1, 12)
}

fn agent_generate_cursor_prefix_lines(manager: &AgentManagerState, create_flow: bool) -> usize {
    let create_tabs = if create_flow { 2 } else { 0 };
    let before_box_content =
        5 + agent_tool_summary_line_count(&manager.draft.tools, &manager.draft.disallow_tools);
    create_tabs + before_box_content
}

fn input_box_cursor_col(value: &str, cursor: usize) -> usize {
    let prefix: String = value.chars().take(cursor).collect();
    2 + prefix.width()
}

fn input_box_inner_width(width: usize) -> usize {
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
            lines: crate::tui::widgets::word_wrap(placeholder, width)
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
    let instruction_lines = crate::tui::widgets::word_wrap(&record.instructions, width);
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
            max_lines: AGENT_EDIT_CONTENT_INSTRUCTIONS_MAX_LINES,
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
                build_agent_model_lines(manager, lines);
                lines.push(Line::from(""));
                editor_summary_row(lines, "工具", agent_tool_summary_text(manager), false);
                pad_lines_to_section_height(lines, AGENT_TOOLS_SECTION_LINES, 1);
            }
            AgentEditorField::Tools => {
                build_agent_tools_lines(manager, lines);
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
        AgentCreateStep::Tools => build_agent_tools_lines(manager, lines),
        AgentCreateStep::Model => build_agent_model_lines(manager, lines),
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
    for line in crate::tui::widgets::word_wrap(description, width)
        .into_iter()
        .take(8)
    {
        lines.push(Line::from(line));
    }
}

fn build_agent_tools_lines(manager: &AgentManagerState, lines: &mut Vec<Line<'static>>) {
    push_agent_section(lines, "工具策略");
    lines.push(Line::from(""));
    let inherited = manager.draft.tools.is_empty();
    lines.push(tool_row(
        manager.tool_selected == 0,
        inherited && manager.draft.disallow_tools.is_empty(),
        "继承全部",
        if manager.draft.disallow_tools.is_empty() {
            "使用父会话可用工具"
        } else {
            "使用父会话工具，排除禁用项"
        },
    ));
    lines.push(Line::from(""));
    push_agent_section(
        lines,
        if inherited {
            "有效工具"
        } else {
            "仅允许"
        },
    );
    lines.push(Line::from(""));
    let allow_rows = [
        ("读工具组", "read", &["read"][..]),
        (
            "写工具组",
            "bash, edit, write",
            &["bash", "edit", "write"][..],
        ),
        ("read", "读取文件", &["read"][..]),
        ("bash", "执行命令", &["bash"][..]),
        ("edit", "编辑文件", &["edit"][..]),
        ("write", "写入文件", &["write"][..]),
        ("ask_user", "询问用户", &["ask_user"][..]),
    ];
    for (offset, (label, desc, tools)) in allow_rows.iter().enumerate() {
        let idx = offset + 1;
        let enabled = tools
            .iter()
            .all(|tool| tool_effectively_enabled(manager, tool));
        lines.push(tool_row(idx == manager.tool_selected, enabled, label, desc));
    }
    lines.push(Line::from(""));
    push_agent_section(lines, "禁用");
    lines.push(Line::from(""));
    let deny_rows = [
        ("read", "不读取文件"),
        ("bash", "不执行命令"),
        ("edit", "不编辑文件"),
        ("write", "不写入文件"),
        ("ask_user", "不询问用户"),
    ];
    for (offset, (label, desc)) in deny_rows.iter().enumerate() {
        let idx = offset + 8;
        let enabled = manager
            .draft
            .disallow_tools
            .iter()
            .any(|tool| tool == label);
        lines.push(tool_row(idx == manager.tool_selected, enabled, label, desc));
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

fn build_agent_model_lines(manager: &AgentManagerState, lines: &mut Vec<Line<'static>>) {
    push_agent_section(lines, "当前配置");
    lines.push(Line::from(""));
    for (idx, entry) in manager.model_entries.iter().enumerate().take(20) {
        match entry {
            AgentModelEntry::Inherit => {
                let selected = idx == manager.model_selected;
                let style = selectable_style(selected);
                lines.push(Line::from(Span::styled(
                    format!(
                        "{} 继承主会话模型{}",
                        if selected { "❯" } else { " " },
                        if manager.draft.model.is_none() {
                            " ✔"
                        } else {
                            ""
                        }
                    ),
                    style,
                )));
                lines.push(Line::from(""));
                push_agent_section(lines, "可选模型");
                lines.push(Line::from(""));
            }
            AgentModelEntry::ProviderHeader { name } => lines.push(Line::from(Span::styled(
                name.clone(),
                Style::default()
                    .fg(Color::Rgb(140, 145, 155))
                    .add_modifier(Modifier::BOLD),
            ))),
            AgentModelEntry::Model {
                provider_key,
                model,
            } => {
                let selected = idx == manager.model_selected;
                let value = format!("{}/{}", provider_key, model.id);
                lines.push(Line::from(format!(
                    "{} {}{}",
                    if selected { "❯" } else { " " },
                    model.name.as_deref().unwrap_or(&model.id),
                    if Some(&value) == manager.draft.model.as_ref() {
                        " ✔"
                    } else {
                        ""
                    }
                )));
            }
        }
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
    Line::from(Span::styled(
        format!(
            "{} {}{:<10} {}",
            if selected { "❯" } else { " " },
            key_part,
            label,
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

struct ModelPanelParams<'a> {
    entries: &'a [ModelSelectionEntry],
    selected: usize,
    thinking_idx: usize,
    active_provider: &'a str,
    active_model: &'a str,
}

fn render_model_panel(
    state: &mut UiState,
    frame: &mut ratatui::Frame,
    area: Rect,
    params: ModelPanelParams<'_>,
) {
    let has_thinking = params
        .entries
        .get(params.selected)
        .is_some_and(|e| matches!(e, ModelSelectionEntry::Model { model, .. } if model.thinking));

    // Layout: entries list + [thinking row] + hint
    let hint_h: u16 = 1;
    let thinking_h: u16 = if has_thinking { 1 } else { 0 };
    let gap_h: u16 = if has_thinking { 1 } else { 0 };
    let list_h = area.height.saturating_sub(hint_h + thinking_h + gap_h);

    let list_area = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: list_h,
    };
    let thinking_y = area.y + list_h + gap_h;
    let hint_y = area.y + area.height - 1;

    // Render model entries
    let mut lines: Vec<Line> = Vec::new();
    let mut model_num: usize = 0;

    for (i, entry) in params.entries.iter().enumerate() {
        if lines.len() >= list_h as usize {
            break;
        }
        match entry {
            ModelSelectionEntry::ProviderHeader { name } => {
                lines.push(Line::from(Span::styled(
                    format!("  {}", name),
                    Style::default()
                        .fg(Color::Rgb(140, 145, 155))
                        .add_modifier(Modifier::BOLD),
                )));
            }
            ModelSelectionEntry::Model {
                provider_key,
                model,
            } => {
                model_num += 1;
                let is_sel = i == params.selected;
                let display = model.name.as_deref().unwrap_or(&model.id);

                // Build description from model config
                let mut desc_parts = Vec::new();
                let limit_k = model.limit / 1000;
                if limit_k > 0 {
                    desc_parts.push(format!("{}K context", limit_k));
                }
                if model.thinking {
                    desc_parts.push("thinking".to_string());
                }
                let desc = desc_parts.join(" · ");

                // Checkmark for non-standard providers (custom models)
                let is_active =
                    provider_key == params.active_provider && model.id == params.active_model;
                let checkmark = if is_active { " ✔" } else { "" };

                let number_str = format!("{}.", model_num);

                let selected_color = Color::Rgb(0x42, 0xd9, 0xe8);
                let active_color = Color::Rgb(126, 158, 126);

                if is_sel {
                    let mut spans = vec![
                        Span::styled(" ❯ ", Style::default().fg(selected_color)),
                        Span::styled(
                            format!(" {} {}{}", number_str, display, checkmark),
                            Style::default()
                                .fg(selected_color)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ];
                    if !desc.is_empty() {
                        spans.push(Span::raw("  "));
                        spans.push(Span::styled(desc, Style::default().fg(selected_color)));
                    }
                    lines.push(Line::from(spans));
                } else {
                    let fg_color = if is_active {
                        active_color
                    } else {
                        Color::Rgb(165, 172, 182)
                    };
                    let style = Style::default().fg(fg_color);
                    let name_style = if is_active {
                        Style::default()
                            .fg(active_color)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        style
                    };
                    let mut spans = vec![
                        Span::styled("   ", style),
                        Span::styled(
                            format!(" {} {}{}", number_str, display, checkmark),
                            name_style,
                        ),
                    ];
                    if !desc.is_empty() {
                        spans.push(Span::raw("  "));
                        spans.push(Span::styled(desc, Style::default().fg(fg_color)));
                    }
                    lines.push(Line::from(spans));
                }
            }
        }
    }

    register_and_highlight_lines(state, list_area, &mut lines);
    frame.render_widget(Paragraph::new(Text::from(lines)), list_area);

    // Thinking effort row
    if has_thinking {
        const EFFORT_ICONS: &[&str] = &["○", "◔", "◑", "◉"];
        const EFFORT_LABELS: &[&str] = &["No", "Low", "Medium", "High"];
        const EFFORT_COLORS: &[Color] = &[
            Color::Rgb(140, 145, 155),
            Color::Rgb(190, 170, 140),
            Color::Rgb(220, 185, 145),
            Color::Rgb(255, 200, 120),
        ];
        let ti = params.thinking_idx.min(EFFORT_ICONS.len() - 1);
        let icon = EFFORT_ICONS[ti];
        let label = EFFORT_LABELS[ti];
        let color = EFFORT_COLORS[ti];

        let thinking_style = Style::default().fg(color).add_modifier(if ti > 0 {
            Modifier::BOLD
        } else {
            Modifier::empty()
        });

        let mut thinking_line = Line::from(vec![
            Span::raw("  "),
            Span::styled(format!("{} {} effort", icon, label), thinking_style),
            Span::raw("   "),
            Span::styled(
                "← → to adjust",
                Style::default().fg(Color::Rgb(140, 145, 155)),
            ),
        ]);
        let thinking_area = Rect {
            x: area.x,
            y: thinking_y,
            width: area.width,
            height: 1,
        };
        register_and_highlight_lines(
            state,
            thinking_area,
            std::slice::from_mut(&mut thinking_line),
        );

        frame.render_widget(Paragraph::new(thinking_line), thinking_area);
    }

    // Hint
    let hint_text = if has_thinking {
        "  ↑↓ select  ·  ←→ effort  ·  Enter confirm  ·  Esc cancel"
    } else {
        "  ↑↓ select  ·  Enter confirm  ·  Esc cancel"
    };
    let mut hint = Line::from(Span::styled(
        hint_text,
        Style::default().fg(Color::Rgb(140, 145, 155)),
    ));
    let hint_area = Rect {
        x: area.x,
        y: hint_y,
        width: area.width,
        height: 1,
    };
    register_and_highlight_lines(state, hint_area, std::slice::from_mut(&mut hint));
    frame.render_widget(Paragraph::new(hint), hint_area);
}

fn render_session_list(state: &mut UiState, frame: &mut ratatui::Frame, area: Rect) {
    let Some(InteractionStep::Session {
        sessions,
        selected,
        search,
        ..
    }) = state.interaction_step.clone()
    else {
        return;
    };

    let total = sessions.len();

    // Layout: header(1) + content(fill) + divider(1) + footer(1)
    let chunks = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(area);

    let header_area = chunks[0];
    let content_area = chunks[1];
    let divider_area = chunks[2];
    let footer_area = chunks[3];

    let content_w = content_area.width as usize;
    let content_h = content_area.height as usize;

    // ── Header ──
    let header_style = Style::default().fg(Color::Rgb(0xa5, 0xac, 0xb6));
    let mut header_lines: Vec<Line> = vec![
        Line::from(Span::styled("  Sessions", header_style)),
        if search.is_empty() {
            Line::from(Span::styled(
                "  Type to Search",
                Style::default().fg(Color::Rgb(0x6a, 0x6a, 0x6a)),
            ))
        } else {
            Line::from(Span::styled(
                format!("  Search: {}", search),
                Style::default().fg(Color::Rgb(0xa5, 0xac, 0xb6)),
            ))
        },
    ];
    register_and_highlight_lines(state, header_area, &mut header_lines);
    frame.render_widget(Paragraph::new(header_lines), header_area);

    // ── Content ──
    let mut lines: Vec<Line> = Vec::with_capacity(content_h);

    if total == 0 {
        // Empty state
        lines.push(Line::from(Span::styled(
            format!("{:width$}", "  No sessions found", width = content_w),
            Style::default().fg(Color::Rgb(0x8a, 0x8a, 0x8a)),
        )));
        while lines.len() < content_h {
            lines.push(Line::from(Span::styled(
                " ".repeat(content_w),
                Style::default(),
            )));
        }
        register_and_highlight_lines(state, content_area, &mut lines);
        frame.render_widget(Paragraph::new(lines), content_area);

        // Divider (empty)
        let divider_style = Style::default().fg(Color::Rgb(0x5a, 0x66, 0x76));
        let mut divider_line = Line::from(Span::styled("─".repeat(content_w), divider_style));
        register_and_highlight_lines(state, divider_area, std::slice::from_mut(&mut divider_line));
        frame.render_widget(Paragraph::new(divider_line), divider_area);

        // Footer
        let mut footer_line = Line::from(Span::styled(
            "  Esc back · Type to search",
            Style::default().fg(Color::Rgb(0x8a, 0x8a, 0x8a)),
        ));
        register_and_highlight_lines(state, footer_area, std::slice::from_mut(&mut footer_line));
        frame.render_widget(Paragraph::new(footer_line), footer_area);
        return;
    }

    // Scroll calculation
    let item_lines = content_h.saturating_sub(2); // reserve top/bottom for indicators
    let mut scroll_off = 0usize;
    if total > item_lines {
        // Keep selected item visible, prefer centering
        let ideal = selected.saturating_sub(item_lines / 2);
        scroll_off = ideal.min(total.saturating_sub(item_lines));
    }
    let show_top = scroll_off > 0;
    let show_bot = scroll_off + item_lines < total;

    let max_visible = item_lines.min(total.saturating_sub(scroll_off));
    let time_width = "2026-05-10 14:30".len() + 2; // time + separator "  "
    let prefix_w = 2; // "❯ " or "  "
    let max_msg_w = content_w.saturating_sub(prefix_w + time_width);

    // ── Build lines ──
    // Top indicator
    if show_top {
        lines.push(Line::from(Span::styled(
            format!("{:width$}", "  ↑ more", width = content_w),
            Style::default().fg(Color::Rgb(0x6a, 0x6a, 0x6a)),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            " ".repeat(content_w),
            Style::default(),
        )));
    }

    // Session items
    for i in 0..max_visible {
        let actual_idx = scroll_off + i;
        let session = &sessions[actual_idx];
        let is_selected = actual_idx == selected;

        let bg = if is_selected {
            Color::Rgb(0x41, 0x45, 0x4c)
        } else if actual_idx.is_multiple_of(2) {
            Color::Rgb(0x33, 0x37, 0x3f)
        } else {
            Color::Reset
        };

        let fg = if is_selected {
            Color::Rgb(0xc1, 0x97, 0x72)
        } else {
            Color::Rgb(0xa5, 0xac, 0xb6)
        };

        let prefix = if is_selected { "❯ " } else { "  " };
        let time_str = relative_time(session.created_at);
        let msg = truncate_str(&session.title, max_msg_w);
        let line_content = format!("{}{}  {}", prefix, time_str, msg);
        let padded = format!("{:<width$}", line_content, width = content_w);

        lines.push(Line::from(Span::styled(
            padded,
            Style::default().fg(fg).bg(bg),
        )));
    }

    // Fill remaining item lines
    while lines.len() < content_h - 1 {
        lines.push(Line::from(Span::styled(
            " ".repeat(content_w),
            Style::default(),
        )));
    }

    // Bottom indicator
    if show_bot {
        lines.push(Line::from(Span::styled(
            format!("{:width$}", "  ↓ more", width = content_w),
            Style::default().fg(Color::Rgb(0x6a, 0x6a, 0x6a)),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            " ".repeat(content_w),
            Style::default(),
        )));
    }

    register_and_highlight_lines(state, content_area, &mut lines);
    frame.render_widget(Paragraph::new(lines), content_area);

    // ── Divider ──
    let current = selected + 1;
    let indicator = format!(" {}/{} ", current, total);
    let dashes_count = content_w.saturating_sub(indicator.len());
    let divider_line = format!("{}{}", "─".repeat(dashes_count), indicator);
    let divider_style = Style::default().fg(Color::Rgb(0x5a, 0x66, 0x76));
    let mut divider_line = Line::from(Span::styled(divider_line, divider_style));
    register_and_highlight_lines(state, divider_area, std::slice::from_mut(&mut divider_line));
    frame.render_widget(Paragraph::new(divider_line), divider_area);

    // ── Footer ──
    let mut footer_line = Line::from(Span::styled(
        "  ↑/↓ navigate · Enter select · Esc back · Type to filter",
        Style::default().fg(Color::Rgb(0x8a, 0x8a, 0x8a)),
    ));
    register_and_highlight_lines(state, footer_area, std::slice::from_mut(&mut footer_line));
    frame.render_widget(Paragraph::new(footer_line), footer_area);
}

/// 将 UTC 时间格式化为相对时间（如 "3m ago", "2h ago", "5d ago"）。
fn relative_time(utc: DateTime<Utc>) -> String {
    let now = Utc::now();
    let duration = now.signed_duration_since(utc);
    let seconds = duration.num_seconds().max(0);
    if seconds < 60 {
        "just now".to_string()
    } else if seconds < 3600 {
        format!("{}m ago", seconds / 60)
    } else if seconds < 86400 {
        format!("{}h ago", seconds / 3600)
    } else if seconds < 604800 {
        format!("{}d ago", seconds / 86400)
    } else if seconds < 2592000 {
        format!("{}w ago", seconds / 604800)
    } else {
        // 超过一个月显示日期
        utc.with_timezone(&Local).format("%m-%d").to_string()
    }
}

/// 截断字符串到指定显示宽度，超长时末尾补 "..."。
fn truncate_str(s: &str, max_width: usize) -> String {
    if max_width == 0 || s.is_empty() {
        return String::new();
    }
    let width = UnicodeWidthStr::width(s);
    if width <= max_width {
        return s.to_string();
    }
    let ellipsis = "...";
    let target = max_width.saturating_sub(UnicodeWidthStr::width(ellipsis));
    let mut result = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = UnicodeWidthChar::width(c).unwrap_or(0);
        if w + cw > target {
            break;
        }
        result.push(c);
        w += cw;
    }
    result.push_str(ellipsis);
    result
}

fn pad_display_width(s: &str, width: usize) -> String {
    let current = UnicodeWidthStr::width(s);
    if current >= width {
        s.to_string()
    } else {
        format!("{}{}", s, " ".repeat(width - current))
    }
}

fn styled_wrapped_text(
    text_block: &TextBlock,
    content_width: usize,
    base_style: Style,
) -> Vec<Line<'static>> {
    styled_wrapped_ranges(&text_block.text, &[], content_width, base_style)
}

fn styled_wrapped_display(
    display: &DisplayMessage,
    content_width: usize,
    base_style: Style,
) -> Vec<Line<'static>> {
    styled_wrapped_ranges(&display.text, &display.mentions, content_width, base_style)
}

fn styled_wrapped_ranges(
    text: &str,
    mentions: &[DisplayMention],
    content_width: usize,
    base_style: Style,
) -> Vec<Line<'static>> {
    if text.is_empty() {
        return Vec::new();
    }

    let mention_ranges = mentions
        .iter()
        .map(|mention| (mention.start_char, mention.end_char, mention.kind))
        .collect::<Vec<_>>();
    let image_ranges = image_marker_ranges(text);
    let mention_style = base_style
        .fg(Color::Rgb(0x42, 0xd9, 0xe8))
        .add_modifier(Modifier::BOLD);
    let normal_style = base_style;
    let width_limit = content_width.max(1);
    let mut lines = Vec::new();

    for logical in split_with_char_offsets(text) {
        let mut spans = Vec::new();
        let mut current_width = 0usize;
        for (char_idx, ch) in logical {
            let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
            if current_width > 0 && current_width + ch_width > width_limit {
                lines.push(Line::from(spans));
                spans = Vec::new();
                current_width = 0;
            }
            let style = if let Some((_, _, kind)) = mention_ranges
                .iter()
                .find(|(start, end, _)| char_idx >= *start && char_idx < *end)
            {
                match kind {
                    MentionKind::Subagent
                    | MentionKind::File
                    | MentionKind::Directory
                    | MentionKind::Command => mention_style,
                }
            } else if image_ranges
                .iter()
                .any(|(start, end)| char_idx >= *start && char_idx < *end)
            {
                mention_style
            } else {
                normal_style
            };
            push_char_span(&mut spans, ch, style);
            current_width += ch_width;
        }
        lines.push(Line::from(spans));
    }

    lines
}

fn image_marker_ranges(text: &str) -> Vec<(usize, usize)> {
    let chars = text.chars().collect::<Vec<_>>();
    let mut ranges = Vec::new();
    let mut idx = 0usize;
    while idx < chars.len() {
        if chars[idx] != '[' {
            idx += 1;
            continue;
        }
        let prefix = ['[', 'I', 'm', 'a', 'g', 'e', '#'];
        if idx + prefix.len() >= chars.len() || chars[idx..idx + prefix.len()] != prefix {
            idx += 1;
            continue;
        }
        let mut end = idx + prefix.len();
        let digit_start = end;
        while end < chars.len() && chars[end].is_ascii_digit() {
            end += 1;
        }
        if end > digit_start && chars.get(end) == Some(&']') {
            ranges.push((idx, end + 1));
            idx = end + 1;
        } else {
            idx += 1;
        }
    }
    ranges
}

fn split_with_char_offsets(text: &str) -> Vec<Vec<(usize, char)>> {
    let mut lines: Vec<Vec<(usize, char)>> = vec![Vec::new()];
    for (idx, ch) in text.chars().enumerate() {
        if ch == '\n' {
            lines.push(Vec::new());
        } else if let Some(line) = lines.last_mut() {
            line.push((idx, ch));
        }
    }
    lines
}

fn push_char_span(spans: &mut Vec<Span<'static>>, ch: char, style: Style) {
    if let Some(last) = spans.last_mut()
        && last.style == style
    {
        last.content.to_mut().push(ch);
        return;
    }
    spans.push(Span::styled(ch.to_string(), style));
}

fn line_width(line: &Line<'_>) -> usize {
    line.spans
        .iter()
        .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
        .sum()
}

fn build_display_message_lines(
    display: &DisplayMessage,
    content_width: usize,
) -> Vec<Line<'static>> {
    let user_bg = Color::Rgb(65, 69, 76);
    let bg_style = Style::default().bg(user_bg);
    let mut lines =
        vec![Line::from(Span::styled(" ".repeat(content_width), bg_style)).style(bg_style)];

    let wrapped = styled_wrapped_display(display, content_width.saturating_sub(2), bg_style);
    if wrapped.is_empty() {
        let text = format!("❯ {}", " ".repeat(content_width.saturating_sub(2)));
        lines.push(Line::from(Span::styled(text, bg_style)).style(bg_style));
    } else {
        for (idx, wl) in wrapped.into_iter().enumerate() {
            let prefix = if idx == 0 { "❯ " } else { "  " };
            let text_width = UnicodeWidthStr::width(prefix) + line_width(&wl);
            let remaining = content_width.saturating_sub(text_width);
            let mut spans = vec![Span::styled(prefix, bg_style)];
            spans.extend(wl.spans);
            spans.push(Span::styled(" ".repeat(remaining), bg_style));
            lines.push(Line::from(spans).style(bg_style));
        }
    }

    lines.push(Line::from(Span::styled(" ".repeat(content_width), bg_style)).style(bg_style));
    lines
}

// ===========================================================================
// Messages, Input, Footer (原逻辑不变)
// ===========================================================================

fn render_subagent_tool(
    tool_use: &ToolUseBlock,
    result: Option<&ToolResultBlock>,
    node: Option<&SubagentNode>,
    content_width: usize,
    project_dir: Option<&Path>,
) -> Vec<Line<'static>> {
    let accent = Color::Rgb(0x42, 0xd9, 0xe8);
    let dim = Color::Rgb(140, 145, 155);
    let text = Color::Rgb(220, 220, 225);
    let label = node
        .map(|node| node.agent_label.as_str())
        .or_else(|| tool_use.input.get("name").and_then(|value| value.as_str()))
        .unwrap_or("Subagent");
    let label = format_subagent_label(label);
    let status = node
        .map(|node| node.status)
        .unwrap_or(SubagentStatus::Running);

    let mut header = vec![Span::raw("· ")];
    if matches!(status, SubagentStatus::Running) {
        header.extend(status::animated_status_spans_with_palette(
            &label,
            accent,
            Color::Rgb(0x1f, 0x4e, 0x58),
        ));
    } else {
        header.push(Span::styled(
            label,
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ));
    }
    if let Some(title) = tool_use
        .input
        .get("title")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|title| !title.is_empty())
    {
        let used_width: usize = header
            .iter()
            .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
            .sum();
        let title_width = content_width.saturating_sub(used_width + 3);
        if title_width >= 8 {
            header.push(Span::styled(" · ", Style::default().fg(dim)));
            header.push(Span::styled(
                truncate_to_width(title, title_width),
                Style::default().fg(dim),
            ));
        }
    }

    let mut lines = vec![Line::from(header)];

    let Some(node) = node else {
        push_subagent_error_lines(&mut lines, result, content_width, "  ");
        return lines;
    };

    let mut seen_tools = HashSet::new();
    let mut child_tools = Vec::new();
    for message in &node.messages {
        for block in &message.content {
            let ContentBlock::ToolUse(child_tool) = block else {
                continue;
            };
            if !seen_tools.insert(child_tool.id.clone()) {
                continue;
            }
            child_tools.push(child_tool);
        }
    }

    let total_tools = child_tools.len();
    let mut rendered_tools = 0usize;
    for (idx, child_tool) in child_tools.iter().enumerate() {
        if total_tools > 6 && idx == 3 {
            lines.push(Line::from(vec![
                Span::raw("     "),
                Span::styled("...", Style::default().fg(dim)),
            ]));
        }
        if total_tools > 6 && idx >= 3 && idx < total_tools.saturating_sub(3) {
            continue;
        }

        let prefix = if rendered_tools == 0 {
            "  └─ "
        } else {
            "     "
        };
        let tool_name = format_tool_label(&child_tool.name);
        let mut spans = vec![
            Span::raw(prefix),
            Span::styled(tool_name, Style::default().fg(text)),
        ];
        if let Some(summary) = subagent_tool_summary(child_tool, project_dir) {
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                truncate_str(&summary, content_width.saturating_sub(10)),
                Style::default().fg(dim),
            ));
        }
        lines.push(Line::from(spans));
        rendered_tools += 1;
    }

    push_subagent_error_lines(&mut lines, result, content_width, "  ");
    lines
}

fn push_subagent_error_lines(
    lines: &mut Vec<Line<'static>>,
    result: Option<&ToolResultBlock>,
    content_width: usize,
    indent: &'static str,
) {
    let Some(result) = result.filter(|result| result.is_error) else {
        return;
    };

    let error_style = Style::default().fg(Color::Rgb(255, 100, 100));
    let content = if result.content.trim().is_empty() {
        "Subagent failed"
    } else {
        result.content.trim()
    };
    let indent_width = UnicodeWidthStr::width(indent);
    let wrapped =
        crate::tui::widgets::word_wrap(content, content_width.saturating_sub(indent_width).max(1));
    for line in wrapped {
        lines.push(Line::from(vec![
            Span::raw(indent),
            Span::styled(line, error_style),
        ]));
    }
}

enum AlertKind {
    Notice,
    Warning,
    Error,
}

fn build_alert_lines(text: &str, content_width: usize, kind: AlertKind) -> Vec<Line<'static>> {
    let (icon, color) = match kind {
        AlertKind::Notice => ("ℹ", Color::Rgb(0x7a, 0xba, 0xff)),
        AlertKind::Warning => ("⚠", Color::Rgb(0xd4, 0xb6, 0x6a)),
        AlertKind::Error => ("✖", Color::Rgb(255, 100, 100)),
    };
    let prefix = format!("{icon} ");
    let wrap_width = content_width
        .saturating_sub(UnicodeWidthStr::width(prefix.as_str()))
        .max(1);
    let wrapped = crate::tui::widgets::word_wrap(text, wrap_width);
    let style = Style::default().fg(color);
    if wrapped.is_empty() {
        return vec![Line::from(vec![Span::styled(prefix, style)])];
    }

    let continuation = " ".repeat(UnicodeWidthStr::width(prefix.as_str()));
    wrapped
        .into_iter()
        .enumerate()
        .map(|(idx, line)| {
            let prefix = if idx == 0 {
                prefix.clone()
            } else {
                continuation.clone()
            };
            Line::from(vec![Span::styled(format!("{prefix}{line}"), style)])
        })
        .collect()
}

fn format_subagent_label(label: &str) -> String {
    let words = label_words(label);
    if words.is_empty() {
        return "Subagent".to_string();
    }

    let mut out = String::new();
    for word in words {
        push_capitalized(&mut out, &word);
    }
    out
}

fn format_tool_label(name: &str) -> String {
    match name {
        "ask_user" => "AskUser".to_string(),
        other => {
            let words = label_words(other);
            if words.is_empty() {
                return other.to_string();
            }

            let mut out = String::new();
            for word in words {
                push_capitalized(&mut out, &word);
            }
            out
        }
    }
}

fn label_words(label: &str) -> Vec<String> {
    label_camel_boundaries(label)
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .map(|part| part.to_ascii_lowercase())
        .collect()
}

fn label_camel_boundaries(label: &str) -> String {
    let mut out = String::new();
    let mut prev: Option<char> = None;

    for ch in label.chars() {
        if let Some(prev) = prev
            && prev.is_ascii_lowercase()
            && ch.is_ascii_uppercase()
        {
            out.push('-');
        }
        out.push(ch);
        prev = Some(ch);
    }

    out
}

fn push_capitalized(out: &mut String, word: &str) {
    let mut chars = word.chars();
    if let Some(first) = chars.next() {
        out.push(first.to_ascii_uppercase());
        out.push_str(chars.as_str());
    }
}

fn subagent_tool_summary(tool_use: &ToolUseBlock, project_dir: Option<&Path>) -> Option<String> {
    match tool_use.name.as_str() {
        "bash" => tool_use
            .input
            .get("command")
            .and_then(|value| value.as_str())
            .map(str::to_string),
        "read" => tool_use
            .input
            .get("file_path")
            .and_then(|value| value.as_str())
            .map(|path| display_path(path, project_dir)),
        "edit" | "write" => tool_use
            .input
            .get("file_path")
            .and_then(|value| value.as_str())
            .map(|path| display_path(path, project_dir)),
        "ask_user" => Some("waiting for user input".to_string()),
        _ => None,
    }
}

fn truncate_to_width(value: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    let width = UnicodeWidthStr::width(value);
    if width <= max_width {
        return value.to_string();
    }
    let mut out = String::new();
    let mut used = 0usize;
    for ch in value.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + ch_width + 1 >= max_width {
            break;
        }
        out.push(ch);
        used += ch_width;
    }
    out.push('…');
    out
}

fn render_messages(state: &mut UiState, frame: &mut ratatui::Frame, area: Rect) {
    if state.messages.is_empty() && state.pending_assistant.is_none() {
        state.selectable_message_lines.clear();
        state.message_scroll_y = 0;
        return;
    }

    let content_width = area.width as usize;
    let visible_height = area.height as usize;
    let mut all_lines: Vec<Line> = Vec::new();
    let mut selectable_lines: Vec<String> = Vec::new();

    let rendered_messages: Vec<&crate::types::message::Message> = state
        .messages
        .iter()
        .filter_map(UiMessage::as_message)
        .collect();

    let mut tool_result_map: HashMap<String, Vec<(usize, usize)>> = HashMap::new();
    for (mi, msg) in rendered_messages.iter().enumerate() {
        for (bi, block) in msg.content.iter().enumerate() {
            if let ContentBlock::ToolResult(tr) = block {
                tool_result_map
                    .entry(tr.tool_use_id.clone())
                    .or_default()
                    .push((mi, bi));
            }
        }
    }
    let mut consumed: HashSet<(usize, usize)> = HashSet::new();

    let mut rendered_msg_idx = 0;
    for ui_message in &state.messages {
        if let UiMessage::Display(display) = ui_message {
            let block_lines = build_display_message_lines(display, content_width);
            if !block_lines.is_empty() {
                if !all_lines.is_empty() {
                    all_lines.push(Line::from(""));
                    selectable_lines.push(String::new());
                }
                selectable_lines.extend(block_lines.iter().map(line_to_plain_text));
                all_lines.extend(block_lines);
            }
            continue;
        }

        let UiMessage::Message(message) = ui_message else {
            let block_lines = match ui_message {
                UiMessage::Notice { text } => {
                    build_alert_lines(text, content_width, AlertKind::Notice)
                }
                UiMessage::Warning { text } => {
                    build_alert_lines(text, content_width, AlertKind::Warning)
                }
                UiMessage::Error { text } => {
                    build_alert_lines(text, content_width, AlertKind::Error)
                }
                UiMessage::Display(_) => unreachable!(),
                UiMessage::Message(_) => unreachable!(),
            };
            if !block_lines.is_empty() {
                if !all_lines.is_empty() {
                    all_lines.push(Line::from(""));
                    selectable_lines.push(String::new());
                }
                selectable_lines.extend(block_lines.iter().map(line_to_plain_text));
                all_lines.extend(block_lines);
            }
            continue;
        };

        let msg_idx = rendered_msg_idx;
        rendered_msg_idx += 1;

        for (block_idx, block) in message.content.iter().enumerate() {
            if let ContentBlock::ToolResult(_) = block
                && consumed.contains(&(msg_idx, block_idx))
            {
                continue;
            }

            let mut block_lines: Vec<Line> = Vec::new();
            match block {
                ContentBlock::Text(tb) if message.role == crate::types::message::Role::User => {
                    let user_bg = Color::Rgb(65, 69, 76);
                    let bg_style = Style::default().bg(user_bg);
                    block_lines.push(
                        Line::from(Span::styled(" ".repeat(content_width), bg_style))
                            .style(bg_style),
                    );

                    let wrapped = styled_wrapped_text(
                        tb,
                        content_width.saturating_sub(2),
                        Style::default().bg(user_bg),
                    );
                    if wrapped.is_empty() {
                        let text = format!("❯ {}", " ".repeat(content_width.saturating_sub(2)));
                        block_lines.push(Line::from(Span::styled(text, bg_style)).style(bg_style));
                    } else {
                        for (idx, wl) in wrapped.into_iter().enumerate() {
                            let prefix = if idx == 0 { "❯ " } else { "  " };
                            let text_width = UnicodeWidthStr::width(prefix) + line_width(&wl);
                            let remaining = content_width.saturating_sub(text_width);
                            let mut spans = vec![Span::styled(prefix, bg_style)];
                            spans.extend(wl.spans);
                            spans.push(Span::styled(" ".repeat(remaining), bg_style));
                            block_lines.push(Line::from(spans).style(bg_style));
                        }
                    }

                    block_lines.push(
                        Line::from(Span::styled(" ".repeat(content_width), bg_style))
                            .style(bg_style),
                    );
                }
                ContentBlock::Text(tb) => {
                    let mut lines = build_plain_lines(&tb.text, content_width);
                    block_lines.append(&mut lines);
                }
                ContentBlock::Image(_) => {}
                ContentBlock::ToolUse(tu) => {
                    if tu.name == "subagent" {
                        let node = state
                            .subagents_by_tool_use
                            .get(&tu.id)
                            .and_then(|session_id| state.subagents.get(session_id));
                        let tool_result = tool_result_map.get(&tu.id).and_then(|positions| {
                            positions.first().and_then(|(mi, bi)| {
                                if let ContentBlock::ToolResult(tr) =
                                    &rendered_messages[*mi].content[*bi]
                                {
                                    Some(tr.clone())
                                } else {
                                    None
                                }
                            })
                        });
                        block_lines.extend(render_subagent_tool(
                            tu,
                            tool_result.as_ref(),
                            node,
                            content_width,
                            Some(state.status_bar.cwd.as_path()),
                        ));
                        if let Some(positions) = tool_result_map.get(&tu.id) {
                            for pos in positions {
                                consumed.insert(*pos);
                            }
                        }
                    } else if let Some(positions) = tool_result_map.get(&tu.id) {
                        let tool_result = positions.first().and_then(|(mi, bi)| {
                            if let ContentBlock::ToolResult(tr) =
                                &rendered_messages[*mi].content[*bi]
                            {
                                Some(tr.clone())
                            } else {
                                None
                            }
                        });

                        let tool_lines = render_tool(
                            tu,
                            tool_result.as_ref(),
                            None,
                            content_width,
                            Some(state.status_bar.cwd.as_path()),
                        );
                        block_lines.extend(tool_lines);

                        for pos in positions {
                            consumed.insert(*pos);
                        }
                    } else {
                        // 工具结果尚未返回
                        let tool_lines = render_tool(
                            tu,
                            None,
                            None,
                            content_width,
                            Some(state.status_bar.cwd.as_path()),
                        );
                        block_lines.extend(tool_lines);
                    }
                }
                ContentBlock::Thinking(tb) => {
                    let mut lines = build_thinking_lines(&tb.thinking, content_width);
                    block_lines.append(&mut lines);
                }
                ContentBlock::ToolResult(tr) => {
                    let color = if tr.is_error {
                        Color::Rgb(255, 100, 100)
                    } else {
                        Color::Rgb(100, 200, 130)
                    };
                    let mut lines =
                        build_bordered_lines(&tr.content, content_width, color, false, None);
                    block_lines.append(&mut lines);
                }
            }

            if !block_lines.is_empty() {
                if !all_lines.is_empty() {
                    all_lines.push(Line::from(""));
                    selectable_lines.push(String::new());
                }
                selectable_lines.extend(block_lines.iter().map(line_to_plain_text));
                all_lines.append(&mut block_lines);
            }
        }
    }

    // ===== 渲染 pending_assistant（流式增量内容） =====
    if let Some(pending) = &state.pending_assistant {
        // 先构建 pending_assistant 内部的 tool_result_map
        let mut tr_indices: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        for (bi, block) in pending.content.iter().enumerate() {
            if let ContentBlock::ToolResult(tr) = block {
                tr_indices.entry(tr.tool_use_id.clone()).or_insert(bi);
            }
        }
        let mut consumed_tr: std::collections::HashSet<usize> = std::collections::HashSet::new();

        for (block_idx, block) in pending.content.iter().enumerate() {
            if let ContentBlock::ToolResult(_) = block
                && consumed_tr.contains(&block_idx)
            {
                continue;
            }

            let mut block_lines: Vec<Line> = Vec::new();
            match block {
                ContentBlock::Text(tb) => {
                    let mut lines = build_plain_lines(&tb.text, content_width);
                    block_lines.append(&mut lines);
                }
                ContentBlock::Image(_) => {}
                ContentBlock::Thinking(tb) => {
                    let mut lines = build_thinking_lines(&tb.thinking, content_width);
                    block_lines.append(&mut lines);
                }
                ContentBlock::ToolUse(tu) => {
                    if tu.name == "subagent" {
                        let node = state
                            .subagents_by_tool_use
                            .get(&tu.id)
                            .and_then(|session_id| state.subagents.get(session_id));
                        let tr = tr_indices.get(&tu.id).and_then(|&bi| {
                            if let ContentBlock::ToolResult(tr) = &pending.content[bi] {
                                Some(tr.clone())
                            } else {
                                None
                            }
                        });
                        block_lines.extend(render_subagent_tool(
                            tu,
                            tr.as_ref(),
                            node,
                            content_width,
                            Some(state.status_bar.cwd.as_path()),
                        ));
                        if let Some(&bi) = tr_indices.get(&tu.id) {
                            consumed_tr.insert(bi);
                        }
                    } else {
                        // 检查是否有对应的 ToolResult
                        let tr = tr_indices.get(&tu.id).and_then(|&bi| {
                            if let ContentBlock::ToolResult(tr) = &pending.content[bi] {
                                consumed_tr.insert(bi);
                                Some(tr.clone())
                            } else {
                                None
                            }
                        });
                        let tool_lines = render_tool(
                            tu,
                            tr.as_ref(),
                            None,
                            content_width,
                            Some(state.status_bar.cwd.as_path()),
                        );
                        block_lines.extend(tool_lines);
                    }
                }
                ContentBlock::ToolResult(tr) => {
                    // 如果没有对应的 ToolUse 来消费它，单独渲染
                    let color = if tr.is_error {
                        Color::Rgb(255, 100, 100)
                    } else {
                        Color::Rgb(100, 200, 130)
                    };
                    let mut lines =
                        build_bordered_lines(&tr.content, content_width, color, false, None);
                    block_lines.append(&mut lines);
                }
            }

            if !block_lines.is_empty() {
                if !all_lines.is_empty() {
                    all_lines.push(Line::from(""));
                    selectable_lines.push(String::new());
                }
                selectable_lines.extend(block_lines.iter().map(line_to_plain_text));
                all_lines.append(&mut block_lines);
            }
        }
    }

    let total_lines = all_lines.len();
    let prev_total_lines = state.total_lines;
    state.total_lines = total_lines;
    if total_lines == 0 {
        return;
    }

    if !state.auto_scroll {
        let delta = total_lines.saturating_sub(prev_total_lines);
        state.scroll_offset = state.scroll_offset.saturating_add(delta);
    }

    let max_scroll = total_lines.saturating_sub(visible_height);
    let capped_offset = state.scroll_offset.min(max_scroll);
    state.scroll_offset = capped_offset;
    let scroll_y = max_scroll.saturating_sub(capped_offset);
    state.selectable_message_lines = selectable_lines;
    state.message_scroll_y = scroll_y;
    let visible_selectable_lines = state
        .selectable_message_lines
        .iter()
        .skip(scroll_y)
        .take(visible_height)
        .cloned()
        .collect::<Vec<_>>();
    for (visible_row, text) in visible_selectable_lines.into_iter().enumerate() {
        state.register_selectable_screen_line(
            area.y + visible_row as u16,
            area.x,
            area.width,
            text,
        );
    }

    let user_bg = Color::Rgb(65, 69, 76);
    let user_line_bg = Style::default().bg(user_bg);
    let user_line_rows = all_lines
        .iter()
        .map(|line| line.style.bg == Some(user_bg))
        .collect::<Vec<_>>();

    apply_text_selection_highlight(state, &mut all_lines, area, scroll_y, visible_height);

    for (idx, is_user_line) in user_line_rows.iter().copied().enumerate().skip(scroll_y) {
        if !is_user_line {
            continue;
        }
        let visible_row = idx - scroll_y;
        if visible_row >= visible_height {
            break;
        }
        frame.buffer_mut().set_style(
            Rect::new(area.x, area.y + visible_row as u16, area.width, 1),
            user_line_bg,
        );
    }

    let paragraph =
        Paragraph::new(ratatui::text::Text::from(all_lines)).scroll((scroll_y as u16, 0));

    frame.render_widget(paragraph, area);
}

fn line_to_plain_text(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>()
}

fn register_and_highlight_lines(state: &mut UiState, area: Rect, lines: &mut [Line<'static>]) {
    for (idx, line) in lines.iter().enumerate() {
        state.register_selectable_screen_line(
            area.y + idx as u16,
            area.x,
            area.width,
            line_to_plain_text(line),
        );
    }

    let highlight = Style::default()
        .fg(Color::Rgb(40, 44, 52))
        .bg(Color::Rgb(180, 210, 255))
        .add_modifier(Modifier::BOLD);

    for (idx, line) in lines.iter_mut().enumerate() {
        let text = line_to_plain_text(line);
        let screen_row = area.y + idx as u16;
        if let Some((start_col, end_col)) = selected_cols_for_screen_line(state, screen_row, &text)
        {
            *line = highlighted_line(&text, start_col, end_col, highlight);
        }
    }
}

fn apply_text_selection_highlight(
    state: &UiState,
    lines: &mut [Line<'static>],
    area: Rect,
    scroll_y: usize,
    visible_height: usize,
) {
    let highlight = Style::default()
        .fg(Color::Rgb(40, 44, 52))
        .bg(Color::Rgb(180, 210, 255))
        .add_modifier(Modifier::BOLD);

    for content_row in scroll_y
        ..state
            .selectable_message_lines
            .len()
            .min(scroll_y.saturating_add(visible_height))
    {
        let Some(text) = state.selectable_message_lines.get(content_row) else {
            continue;
        };
        let screen_row = area.y + (content_row - scroll_y) as u16;
        if let (Some((start_col, end_col)), Some(line)) = (
            selected_cols_for_screen_line(state, screen_row, text),
            lines.get_mut(content_row),
        ) {
            *line = highlighted_line(text, start_col, end_col, highlight);
        }
    }
}
