use crate::tui::state::{AgentStatus, InteractionStep, ModelSelectionEntry, UiState};
use crate::tui::widgets::{
    build_bordered_lines, build_plain_lines, build_thinking_lines, render_tool,
};
use crate::types::events::{PermissionPreview, ToolPauseKind, ToolPauseRequest};
use crate::types::message::{ContentBlock, ToolUseBlock};
use chrono::DateTime;
use chrono::Local;
use chrono::Utc;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Clear, Paragraph};
use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const PERMISSION_DRAWER_MAX_HEIGHT: u16 = 18;
const EDIT_PERMISSION_DRAWER_MAX_HEIGHT: u16 = 50;

pub fn render(state: &mut UiState, frame: &mut ratatui::Frame) {
    let area = frame.area();
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

    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .split(area);
    state.messages_area = chunks[1];

    render_messages(state, frame, chunks[1]);
    render_autocomplete(state, frame, chunks[3]);
    render_footer(state, frame, chunks[4]);

    // Draw input box only when no modal interaction is active (prevents cursor showing through overlay)
    if state.interaction_step.is_none() && state.active_tool_pause().is_none() {
        render_input(state, frame, chunks[3]);
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

    let tool_use = find_tool_use(state, &request.tool_use_id);
    let content_width = area.width.saturating_sub(6) as usize;
    let lines = build_permission_drawer_lines(&request, tool_use, content_width);
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
    let scroll_offset = state.permission_scroll_offset.min(max_scroll);
    state.permission_scroll_offset = scroll_offset;
    state.permission_drawer_content_len = scroll_lines.len();
    let visible_lines: Vec<Line<'static>> = scroll_lines
        .into_iter()
        .skip(scroll_offset)
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

    let title = " Permission Request ".to_string();
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
        render_permission_scrollbar(frame, body_area, scroll_offset, scroll_line_count);
    }

    let yes_style = permission_option_style(state.permission_selected == 0);
    let no_style = permission_option_style(state.permission_selected == 1);
    let (yes_desc, no_desc) = permission_option_descriptions(&request);
    let desc_style = Style::default().fg(Color::Rgb(140, 145, 155));
    let options = Text::from(vec![
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
    ]);
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

fn render_permission_scrollbar(
    frame: &mut ratatui::Frame,
    body_area: Rect,
    scroll_offset: usize,
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
        scroll_offset.saturating_mul(thumb_range) / max_scroll
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
            .fg(Color::Rgb(135, 135, 255))
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
        ToolPauseKind::Permission(PermissionPreview::ApplyPatch(_)) => {
            ("apply patch", "reject patch")
        }
        ToolPauseKind::Permission(PermissionPreview::Read(_)) => ("read file", "skip read"),
        ToolPauseKind::Permission(PermissionPreview::Custom { .. }) => ("allow tool", "deny tool"),
        ToolPauseKind::UserInput(_) => ("submit response", "cancel request"),
    }
}

fn build_permission_drawer_lines(
    request: &ToolPauseRequest,
    tool_use: Option<&ToolUseBlock>,
    content_width: usize,
) -> Vec<Line<'static>> {
    match &request.kind {
        ToolPauseKind::Permission(PermissionPreview::Bash(preview)) => {
            let mut lines = Vec::new();
            lines.push(Line::from(vec![
                Span::raw("· "),
                Span::styled(
                    "Bash",
                    Style::default()
                        .fg(Color::Rgb(0x42, 0xb3, 0xc2))
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" command", Style::default().fg(Color::Rgb(165, 172, 182))),
            ]));
            lines.push(Line::from(""));

            if let Some(description) = &preview.description
                && !description.trim().is_empty()
            {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(
                        format!("# {}", description),
                        Style::default()
                            .fg(Color::Rgb(140, 145, 155))
                            .add_modifier(Modifier::ITALIC),
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
            lines
        }
        ToolPauseKind::Permission(PermissionPreview::Edit(_)) => {
            if let Some(tool_use) = tool_use {
                render_tool(tool_use, None, Some(request), content_width, false)
            } else {
                vec![Line::from(Span::styled(
                    "Missing edit tool input for preview",
                    Style::default().fg(Color::Rgb(255, 100, 100)),
                ))]
            }
        }
        ToolPauseKind::Permission(PermissionPreview::Write(_)) => {
            if let Some(tool_use) = tool_use {
                render_tool(tool_use, None, Some(request), content_width, false)
            } else {
                vec![Line::from(Span::styled(
                    "Missing write tool input for preview",
                    Style::default().fg(Color::Rgb(255, 100, 100)),
                ))]
            }
        }
        ToolPauseKind::Permission(preview) => vec![Line::from(Span::styled(
            format!("{} permission requested", permission_name(preview)),
            Style::default().fg(Color::Rgb(165, 172, 182)),
        ))],
        ToolPauseKind::UserInput(preview) => {
            crate::tui::widgets::word_wrap(&preview.prompt, content_width)
                .into_iter()
                .map(Line::from)
                .collect()
        }
    }
}

fn permission_name(preview: &PermissionPreview) -> &'static str {
    match preview {
        PermissionPreview::Bash(_) => "bash",
        PermissionPreview::ApplyPatch(_) => "apply_patch",
        PermissionPreview::Edit(_) => "edit",
        PermissionPreview::Write(_) => "write",
        PermissionPreview::Read(_) => "read",
        PermissionPreview::Custom { .. } => "custom tool",
    }
}

fn find_tool_use<'a>(state: &'a UiState, tool_use_id: &str) -> Option<&'a ToolUseBlock> {
    state
        .pending_assistant
        .iter()
        .flat_map(|m| m.content.iter())
        .chain(state.messages.iter().flat_map(|m| m.content.iter()))
        .find_map(|block| match block {
            ContentBlock::ToolUse(tu) if tu.id == tool_use_id => Some(tu),
            _ => None,
        })
}

fn render_autocomplete(state: &UiState, frame: &mut ratatui::Frame, input_area: Rect) {
    if !state.autocomplete.visible || state.autocomplete.filtered.is_empty() {
        return;
    }

    let max_items = 6;
    let count = state.autocomplete.filtered.len().min(max_items);
    let popup_width = input_area.width;

    let popup_height = count as u16;
    let y = input_area.y.saturating_sub(popup_height);
    let popup_area = Rect {
        x: input_area.x,
        y,
        width: popup_width,
        height: popup_height,
    };

    frame.render_widget(Clear, popup_area);

    // 整体背景：输入框底色
    let input_bg = Color::Rgb(65, 69, 76);
    frame.render_widget(
        Paragraph::new(Line::from("")).style(Style::default().bg(input_bg)),
        popup_area,
    );

    // 边框色
    let border_clr = Color::Rgb(90, 102, 118);
    let sel_bg = Color::Rgb(255, 204, 163);
    let idle_fg = Color::Rgb(165, 172, 182);

    // ┃ + 2spaces + 内容 + 2spaces + ┃
    let content_width = popup_width.saturating_sub(4) as usize;

    let lines: Vec<Line> = {
        let cmds: Vec<_> = state.autocomplete.filtered.iter().collect();
        let max_name_width = cmds
            .iter()
            .map(|cmd| format!("/{}", cmd.name).chars().count())
            .max()
            .unwrap_or(0);
        cmds.into_iter()
            .enumerate()
            .map(|(i, cmd)| {
                let is_sel = i == state.autocomplete.selected;
                let row_bg = if is_sel { sel_bg } else { input_bg };
                let row_fg = idle_fg;

                let left = format!("/{}", cmd.name);
                let padding = " ".repeat(max_name_width.saturating_sub(left.chars().count()));
                let text = format!("{}{}  {}", left, padding, cmd.description);
                let text_w = UnicodeWidthStr::width(&text[..]);
                let pad = content_width.saturating_sub(text_w);

                // 边框 ┃ 不设 bg，透出 input_bg；内容区用 row_bg（选中时高亮）
                let content_style = Style::default().fg(row_fg).bg(row_bg);
                let border_style = Style::default().fg(border_clr);

                let mut spans = vec![
                    Span::styled("\u{2503}", border_style),
                    Span::styled(text, content_style),
                ];
                if pad > 0 {
                    spans.push(Span::styled(" ".repeat(pad), content_style));
                }
                spans.push(Span::styled("  ", content_style));
                spans.push(Span::styled("\u{2503}", border_style));

                Line::from(spans)
            })
            .collect()
    };

    frame.render_widget(Paragraph::new(ratatui::text::Text::from(lines)), popup_area);
}

// ===========================================================================
// 交互选择页
// ===========================================================================

fn render_interaction(state: &UiState, frame: &mut ratatui::Frame, area: Rect) {
    // ThinkingEffort is now inlined inside ModelSelection
    let Some(InteractionStep::ModelSelection {
        entries,
        selected,
        thinking_idx,
        active_provider,
        active_model,
    }) = &state.interaction_step
    else {
        return;
    };

    // Panel height
    let has_thinking = entries
        .get(*selected)
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
    let accent = Color::Rgb(135, 135, 255);

    // Line 0: thick divider above the panel (━ characters, accent color)
    let divider_line = Line::from(Span::styled(
        "━".repeat(panel_area.width.saturating_sub(1) as usize),
        Style::default().fg(accent),
    ));

    frame.render_widget(
        Paragraph::new(divider_line),
        Rect {
            x: panel_area.x,
            y: panel_area.y.saturating_sub(1),
            width: panel_area.width,
            height: 1,
        },
    );

    // Line 1: "Select model" in accent color, bold
    let title_line = Line::from(Span::styled(
        " Select model",
        Style::default().fg(accent).add_modifier(Modifier::BOLD),
    ));

    frame.render_widget(
        Paragraph::new(title_line),
        Rect {
            x: panel_area.x,
            y: panel_area.y + 1,
            width: panel_area.width,
            height: 1,
        },
    );

    // Line 2: Chinese subtitle in gray
    let subtitle_line = Line::from(Span::styled(
        " 切换模型，适用于当前会话和未来会话。",
        Style::default().fg(Color::Rgb(140, 145, 155)),
    ));

    frame.render_widget(
        Paragraph::new(subtitle_line),
        Rect {
            x: panel_area.x,
            y: panel_area.y + 2,
            width: panel_area.width,
            height: 1,
        },
    );

    // Content area below divider
    let content_area = Rect {
        x: panel_area.x,
        y: panel_area.y + 3,
        width: panel_area.width,
        height: panel_area.height - 3,
    };

    render_model_panel(
        frame,
        content_area,
        entries,
        *selected,
        *thinking_idx,
        active_provider,
        active_model,
    );
}

fn render_model_panel(
    frame: &mut ratatui::Frame,
    area: Rect,
    entries: &[ModelSelectionEntry],
    selected: usize,
    thinking_idx: usize,
    active_provider: &str,
    active_model: &str,
) {
    let has_thinking = entries
        .get(selected)
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

    for (i, entry) in entries.iter().enumerate() {
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
                let is_sel = i == selected;
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
                let is_active = provider_key == active_provider && model.id == active_model;
                let checkmark = if is_active { " ✔" } else { "" };

                let number_str = format!("{}.", model_num);

                let selected_color = Color::Rgb(135, 135, 255);
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
        let ti = thinking_idx.min(EFFORT_ICONS.len() - 1);
        let icon = EFFORT_ICONS[ti];
        let label = EFFORT_LABELS[ti];
        let color = EFFORT_COLORS[ti];

        let thinking_style = Style::default().fg(color).add_modifier(if ti > 0 {
            Modifier::BOLD
        } else {
            Modifier::empty()
        });

        let thinking_line = Line::from(vec![
            Span::raw("  "),
            Span::styled(format!("{} {} effort", icon, label), thinking_style),
            Span::raw("   "),
            Span::styled(
                "← → to adjust",
                Style::default().fg(Color::Rgb(140, 145, 155)),
            ),
        ]);

        frame.render_widget(
            Paragraph::new(thinking_line),
            Rect {
                x: area.x,
                y: thinking_y,
                width: area.width,
                height: 1,
            },
        );
    }

    // Hint
    let hint_text = if has_thinking {
        "  ↑↓ select  ·  ←→ effort  ·  Enter confirm  ·  Esc cancel"
    } else {
        "  ↑↓ select  ·  Enter confirm  ·  Esc cancel"
    };
    let hint = Line::from(Span::styled(
        hint_text,
        Style::default().fg(Color::Rgb(140, 145, 155)),
    ));
    frame.render_widget(
        Paragraph::new(hint),
        Rect {
            x: area.x,
            y: hint_y,
            width: area.width,
            height: 1,
        },
    );
}

fn render_session_list(state: &UiState, frame: &mut ratatui::Frame, area: Rect) {
    let Some(InteractionStep::Session {
        sessions,
        selected,
        search,
        ..
    }) = &state.interaction_step
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
    let header_lines: Vec<Line> = vec![
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
        frame.render_widget(Paragraph::new(lines), content_area);

        // Divider (empty)
        let divider_style = Style::default().fg(Color::Rgb(0x5a, 0x66, 0x76));
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "─".repeat(content_w),
                divider_style,
            ))),
            divider_area,
        );

        // Footer
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "  Esc back · Type to search",
                Style::default().fg(Color::Rgb(0x8a, 0x8a, 0x8a)),
            ))),
            footer_area,
        );
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
        let is_selected = actual_idx == *selected;

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

    frame.render_widget(Paragraph::new(lines), content_area);

    // ── Divider ──
    let current = *selected + 1;
    let indicator = format!(" {}/{} ", current, total);
    let dashes_count = content_w.saturating_sub(indicator.len());
    let divider_line = format!("{}{}", "─".repeat(dashes_count), indicator);
    let divider_style = Style::default().fg(Color::Rgb(0x5a, 0x66, 0x76));
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(divider_line, divider_style))),
        divider_area,
    );

    // ── Footer ──
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "  ↑/↓ navigate · Enter select · Esc back · Type to filter",
            Style::default().fg(Color::Rgb(0x8a, 0x8a, 0x8a)),
        ))),
        footer_area,
    );
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

// ===========================================================================
// Messages, Input, Footer (原逻辑不变)
// ===========================================================================

fn render_messages(state: &mut UiState, frame: &mut ratatui::Frame, area: Rect) {
    if state.messages.is_empty() && state.pending_assistant.is_none() {
        return;
    }

    let content_width = area.width as usize;
    let visible_height = area.height as usize;
    state.block_ranges.clear();

    let mut all_lines: Vec<Line> = Vec::new();

    let mut tool_result_map: HashMap<String, Vec<(usize, usize)>> = HashMap::new();
    for (mi, msg) in state.messages.iter().enumerate() {
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

    for (msg_idx, message) in state.messages.iter().enumerate() {
        for (block_idx, block) in message.content.iter().enumerate() {
            if let ContentBlock::ToolResult(_) = block
                && consumed.contains(&(msg_idx, block_idx))
            {
                continue;
            }

            let mut block_lines: Vec<Line> = Vec::new();
            let mut block_tool_id: Option<String> = None;

            match block {
                ContentBlock::Text(tb) if message.role == crate::types::message::Role::User => {
                    let user_bg = Color::Rgb(65, 69, 76);
                    let bg_style = Style::default().bg(user_bg);
                    block_lines.push(Line::from(Span::styled(
                        " ".repeat(content_width),
                        bg_style,
                    )));

                    let wrapped =
                        crate::tui::widgets::word_wrap(&tb.text, content_width.saturating_sub(2));
                    if wrapped.is_empty() {
                        let text = format!("❯ {}", " ".repeat(content_width.saturating_sub(2)));
                        block_lines.push(Line::from(Span::styled(text, bg_style)));
                    } else {
                        for (idx, wl) in wrapped.iter().enumerate() {
                            let prefix = if idx == 0 { "❯ " } else { "  " };
                            let text = format!("{}{}", prefix, wl);
                            let text_width = UnicodeWidthStr::width(&*text);
                            let remaining = content_width.saturating_sub(text_width);
                            let full_line = format!("{}{}", text, " ".repeat(remaining));
                            block_lines.push(Line::from(Span::styled(full_line, bg_style)));
                        }
                    }

                    block_lines.push(Line::from(Span::styled(
                        " ".repeat(content_width),
                        bg_style,
                    )));
                }
                ContentBlock::Text(tb) => {
                    let mut lines = build_plain_lines(&tb.text, content_width);
                    block_lines.append(&mut lines);
                }
                ContentBlock::ToolUse(tu) => {
                    if let Some(positions) = tool_result_map.get(&tu.id) {
                        let tool_result = positions.first().and_then(|(mi, bi)| {
                            if let ContentBlock::ToolResult(tr) = &state.messages[*mi].content[*bi]
                            {
                                Some(tr.clone())
                            } else {
                                None
                            }
                        });

                        let collapsed = !state.expanded_tools.contains(&tu.id);
                        let tool_lines =
                            render_tool(tu, tool_result.as_ref(), None, content_width, collapsed);
                        block_lines.extend(tool_lines);

                        block_tool_id = Some(tu.id.clone());

                        for pos in positions {
                            consumed.insert(*pos);
                        }
                    } else {
                        // 工具结果尚未返回
                        let tool_lines = render_tool(tu, None, None, content_width, false);
                        block_lines.extend(tool_lines);
                        block_tool_id = Some(tu.id.clone());
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
                }
                let base = all_lines.len();
                let block_len = block_lines.len();
                if let Some(tool_id) = block_tool_id.take() {
                    state.block_ranges.push((base..base + block_len, tool_id));
                }
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
            let mut block_tool_id: Option<String> = None;

            match block {
                ContentBlock::Text(tb) => {
                    let mut lines = build_plain_lines(&tb.text, content_width);
                    block_lines.append(&mut lines);
                }
                ContentBlock::Thinking(tb) => {
                    let mut lines = build_thinking_lines(&tb.thinking, content_width);
                    block_lines.append(&mut lines);
                }
                ContentBlock::ToolUse(tu) => {
                    // 检查是否有对应的 ToolResult
                    let tr = tr_indices.get(&tu.id).and_then(|&bi| {
                        if let ContentBlock::ToolResult(tr) = &pending.content[bi] {
                            consumed_tr.insert(bi);
                            Some(tr.clone())
                        } else {
                            None
                        }
                    });
                    let tool_lines = render_tool(tu, tr.as_ref(), None, content_width, false);
                    block_lines.extend(tool_lines);
                    block_tool_id = Some(tu.id.clone());
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
                }
                let base = all_lines.len();
                let block_len = block_lines.len();
                if let Some(tool_id) = block_tool_id.take() {
                    state.block_ranges.push((base..base + block_len, tool_id));
                }
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

    let paragraph =
        Paragraph::new(ratatui::text::Text::from(all_lines)).scroll((scroll_y as u16, 0));

    frame.render_widget(paragraph, area);
}

fn render_input(state: &UiState, frame: &mut ratatui::Frame, area: Rect) {
    let bg = Style::default().bg(Color::Rgb(65, 69, 76));

    let bg_widget = Paragraph::new(Line::from("")).style(bg);
    frame.render_widget(bg_widget, area);

    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(area);
    let input_line = chunks[1];

    let line_bg = Paragraph::new(Line::from(Span::styled(
        " ".repeat(area.width as usize),
        bg,
    )))
    .style(bg);
    frame.render_widget(line_bg, input_line);

    let prefix_style = Style::default().fg(Color::Rgb(0xab, 0xab, 0xab));
    let cmd_color = Style::default().fg(Color::Rgb(0x7c, 0x5c, 0xf6));
    let placeholder_style = Style::default().fg(Color::DarkGray);

    let content = if state.input.is_empty() {
        Line::from(vec![
            Span::styled("\u{276f} ", prefix_style),
            Span::styled("Type a message...", placeholder_style),
        ])
    } else {
        let input = &state.input;
        let command_matched = input
            .starts_with('/')
            .then(|| {
                let cmd_raw = if let Some(space_pos) = input.find(' ') {
                    &input[1..space_pos]
                } else {
                    &input[1..]
                };
                state
                    .autocomplete
                    .all_commands
                    .iter()
                    .find(|c| c.name == cmd_raw || c.aliases.iter().any(|a| a == cmd_raw))
            })
            .flatten();

        if let Some(cmd) = command_matched {
            let after_cmd = if let Some(space_pos) = input.find(' ') {
                &input[space_pos..]
            } else {
                ""
            };
            let mut spans = vec![
                Span::styled("\u{276f} ", prefix_style),
                Span::styled(format!("/{}", cmd.name), cmd_color),
            ];
            if !after_cmd.is_empty() {
                spans.push(Span::raw(after_cmd));
                // 有参数的命令：输入空格后显示 <args_description> 占位提示
                if cmd.has_args
                    && after_cmd == " "
                    && let Some(ref desc) = cmd.args_description
                {
                    spans.push(Span::styled(format!("<{}>", desc), placeholder_style));
                }
            }
            Line::from(spans)
        } else {
            Line::from(vec![
                Span::styled("\u{276f} ", prefix_style),
                Span::raw(input),
            ])
        }
    };
    let paragraph = Paragraph::new(content);
    frame.render_widget(paragraph, input_line);

    let cursor_x = if state.input.is_empty() {
        input_line.x + 2
    } else {
        let byte_idx = state.char_to_byte(state.cursor_char);
        let prefix_width = UnicodeWidthStr::width(&state.input[..byte_idx]);
        input_line.x + 2 + prefix_width as u16
    };
    frame.set_cursor_position((cursor_x, input_line.y));
}

fn animated_status_spans(text: &str) -> Vec<Span<'static>> {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    if n == 0 {
        return vec![];
    }

    // 基于时间计算波位置：每 ~2s 循环一次
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as f64;
    const CYCLE_MS: f64 = 1200.0;
    let phase = (ms % CYCLE_MS) / CYCLE_MS; // 0.0 → 1.0
    let wave_pos = phase * n as f64; // 0 → n

    // 亮色（基色 #c8a9ee）
    const BR: u8 = 200;
    const BG: u8 = 169;
    const BB: u8 = 238;
    // 暗色（灰紫色）
    const DR: u8 = 55;
    const DG: u8 = 47;
    const DB: u8 = 65;

    chars
        .iter()
        .enumerate()
        .map(|(i, &c)| {
            // 字符 i 到波峰的有向距离（绕环处理）
            let diff = ((i as f64 - wave_pos + n as f64) % n as f64) - n as f64 / 2.0;
            let normalized = diff / (n as f64 / 2.0); // -1 … 1
            // 余弦钟形：中心 1.0，边缘 0.0
            let bell = (normalized * std::f64::consts::PI).cos().max(0.0);
            // 保证最暗也有微弱可见度
            let dim_min = 0.08;
            let brightness = dim_min + (1.0 - dim_min) * bell;

            let r = (DR as f64 + (BR as f64 - DR as f64) * brightness) as u8;
            let g = (DG as f64 + (BG as f64 - DG as f64) * brightness) as u8;
            let b = (DB as f64 + (BB as f64 - DB as f64) * brightness) as u8;

            Span::styled(c.to_string(), Style::default().fg(Color::Rgb(r, g, b)))
        })
        .collect()
}

fn render_footer(state: &UiState, frame: &mut ratatui::Frame, area: Rect) {
    use crate::types::config::ThinkingEffort;

    let path_display = {
        let cwd_str = state.status_bar.cwd.to_string_lossy();
        let home = std::env::var("HOME").unwrap_or_default();
        if !home.is_empty() && cwd_str.starts_with(&home) {
            format!("~{}", &cwd_str[home.len()..])
        } else {
            cwd_str.to_string()
        }
    };

    let model_part = if state.status_bar.active_provider.is_empty() {
        state.status_bar.model.clone()
    } else {
        format!(
            "{}/{}",
            state.status_bar.active_provider, state.status_bar.model
        )
    };

    let model_thinking = match state.status_bar.thinking_effort {
        Some(ThinkingEffort::Low) => format!("{}  low", model_part),
        Some(ThinkingEffort::Medium) => format!("{}  medium", model_part),
        Some(ThinkingEffort::High) => format!("{}  high", model_part),
        _ => model_part,
    };

    let mut base_spans: Vec<Span<'static>> = vec![
        Span::raw(" "),
        Span::styled(
            format!(" {} ", model_thinking),
            Style::default().fg(Color::Rgb(0xf6, 0xe2, 0xb7)),
        ),
        Span::styled("·", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!(" {} ", path_display),
            Style::default().fg(Color::Rgb(0xab, 0xdf, 0xa7)),
        ),
        Span::styled("·", Style::default().fg(Color::DarkGray)),
    ];

    let line = match &state.agent_status {
        AgentStatus::Thinking | AgentStatus::Working => {
            let mut spans = base_spans;
            spans.push(Span::raw(" "));
            spans.extend(animated_status_spans(&state.agent_status.to_string()));
            spans.push(Span::raw(" "));
            Line::from(spans)
        }
        _ => {
            base_spans.push(Span::styled(
                format!(" {} ", state.agent_status),
                Style::default().fg(Color::Rgb(0xc8, 0xa9, 0xee)),
            ));
            Line::from(base_spans)
        }
    };

    let paragraph = Paragraph::new(line).style(Style::default().fg(Color::DarkGray));
    frame.render_widget(paragraph, area);
}
