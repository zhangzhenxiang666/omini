use crate::state::UiState;
use crate::types::events::CommandSummary;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

pub(super) fn render_autocomplete(state: &UiState, frame: &mut ratatui::Frame, input_area: Rect) {
    if state.mention_autocomplete.visible {
        render_mentions(state, frame, input_area);
        return;
    }

    if !state.autocomplete.visible || state.autocomplete.filtered.is_empty() {
        return;
    }

    let max_items = 6;
    let total = state.autocomplete.filtered.len();
    let selected = state.autocomplete.selected.min(total.saturating_sub(1));
    let start = if selected >= max_items {
        selected + 1 - max_items
    } else {
        0
    };
    let count = total.saturating_sub(start).min(max_items);
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

    let input_bg = Color::Rgb(65, 69, 76);
    frame.render_widget(
        Paragraph::new(Line::from("")).style(Style::default().bg(input_bg)),
        popup_area,
    );

    let border_clr = Color::Rgb(90, 102, 118);
    let sel_bg = Color::Rgb(255, 204, 163);
    let idle_fg = Color::Rgb(165, 172, 182);
    let content_width = popup_width.saturating_sub(4) as usize;

    let cmds: Vec<_> = state.autocomplete.filtered.iter().collect();
    let max_name_width = cmds
        .iter()
        .map(|cmd| UnicodeWidthStr::width(command_signature(cmd).as_str()))
        .max()
        .unwrap_or(0);
    let lines: Vec<Line> = cmds
        .into_iter()
        .skip(start)
        .take(count)
        .enumerate()
        .map(|(i, cmd)| {
            let is_sel = start + i == selected;
            let row_bg = if is_sel { sel_bg } else { input_bg };
            let content_style = Style::default().fg(idle_fg).bg(row_bg);
            let border_style = Style::default().fg(border_clr);

            command_row_line(
                cmd,
                max_name_width,
                content_width,
                content_style,
                border_style,
            )
        })
        .collect();

    frame.render_widget(Paragraph::new(ratatui::text::Text::from(lines)), popup_area);
}

fn render_mentions(state: &UiState, frame: &mut ratatui::Frame, input_area: Rect) {
    let max_items = 8;
    let total = state.mention_autocomplete.filtered.len();
    let selected = state
        .mention_autocomplete
        .selected
        .min(total.saturating_sub(1));
    let start = if selected >= max_items {
        selected + 1 - max_items
    } else {
        0
    };
    let count = total.saturating_sub(start).min(max_items).max(1);
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

    let input_bg = Color::Rgb(65, 69, 76);
    frame.render_widget(
        Paragraph::new(Line::from("")).style(Style::default().bg(input_bg)),
        popup_area,
    );

    let border_clr = Color::Rgb(90, 102, 118);
    let sel_bg = Color::Rgb(255, 204, 163);
    let idle_fg = Color::Rgb(165, 172, 182);
    let kind_fg = Color::Rgb(0x42, 0xd9, 0xe8);
    let content_width = popup_width.saturating_sub(4) as usize;

    if total == 0 {
        let text = "无匹配";
        let text_w = UnicodeWidthStr::width(text);
        let pad = content_width.saturating_sub(text_w);
        let content_style = Style::default().fg(idle_fg).bg(input_bg);
        let border_style = Style::default().fg(border_clr);
        let mut spans = vec![
            Span::styled("\u{2503}", border_style),
            Span::styled(text.to_string(), content_style),
        ];
        if pad > 0 {
            spans.push(Span::styled(" ".repeat(pad), content_style));
        }
        spans.push(Span::styled("  ", content_style));
        spans.push(Span::styled("\u{2503}", border_style));
        frame.render_widget(
            Paragraph::new(ratatui::text::Text::from(vec![Line::from(spans)])),
            popup_area,
        );
        return;
    }

    let candidates: Vec<_> = state.mention_autocomplete.filtered.iter().collect();
    let max_name_width = candidates
        .iter()
        .map(|candidate| UnicodeWidthStr::width(candidate.drawer_display().as_str()))
        .max()
        .unwrap_or(0);
    let lines: Vec<Line> = candidates
        .into_iter()
        .skip(start)
        .take(count)
        .enumerate()
        .map(|(i, candidate)| {
            let is_sel = start + i == selected;
            let row_bg = if is_sel { sel_bg } else { input_bg };
            let left = candidate.drawer_display();
            let padding =
                " ".repeat(max_name_width.saturating_sub(UnicodeWidthStr::width(left.as_str())));
            let kind = match candidate.kind {
                omini_domain::display::MentionKind::Subagent => "agent",
                omini_domain::display::MentionKind::Directory => "目录",
                omini_domain::display::MentionKind::File if candidate.description == "image" => {
                    "图片"
                }
                omini_domain::display::MentionKind::File => "文件",
                omini_domain::display::MentionKind::Command => "命令",
            };
            let description = mention_description(candidate.description.as_str());
            let kind_display = pad_display_width(kind, 5);

            let content_style = Style::default().fg(idle_fg).bg(row_bg);
            let kind_style = Style::default().fg(kind_fg).bg(row_bg);
            let border_style = Style::default().fg(border_clr);
            let mut spans = vec![Span::styled("\u{2503}", border_style)];
            let mut remaining = content_width;
            push_clipped_span(
                &mut spans,
                &format!("{}{}  ", left, padding),
                content_style,
                &mut remaining,
            );
            push_clipped_span(&mut spans, &kind_display, kind_style, &mut remaining);
            push_clipped_span(
                &mut spans,
                &format!("  {}", description),
                content_style,
                &mut remaining,
            );
            finish_bordered_row(&mut spans, remaining, content_style, border_style);
            Line::from(spans)
        })
        .collect();

    frame.render_widget(Paragraph::new(ratatui::text::Text::from(lines)), popup_area);
}

fn pad_display_width(text: &str, width: usize) -> String {
    let text_width = UnicodeWidthStr::width(text);
    if text_width >= width {
        text.to_string()
    } else {
        format!("{}{}", text, " ".repeat(width - text_width))
    }
}

fn mention_description(description: &str) -> &str {
    match description {
        "image" => "图片",
        "file" => "文件",
        "directory" => "目录",
        "command" => "命令",
        other => other,
    }
}

fn command_row_line(
    cmd: &CommandSummary,
    max_name_width: usize,
    content_width: usize,
    content_style: Style,
    border_style: Style,
) -> Line<'static> {
    let left = command_signature(cmd);
    let padding = " ".repeat(max_name_width.saturating_sub(UnicodeWidthStr::width(left.as_str())));
    let text = format!("{}{}  {}", left, padding, cmd.description);
    let mut spans = vec![Span::styled("\u{2503}", border_style)];
    let mut remaining = content_width;
    push_clipped_span(&mut spans, &text, content_style, &mut remaining);
    finish_bordered_row(&mut spans, remaining, content_style, border_style);
    Line::from(spans)
}

fn command_signature(cmd: &CommandSummary) -> String {
    if cmd.has_args
        && let Some(args) = cmd.args_description.as_deref()
    {
        format!("/{} {}", cmd.name, args)
    } else {
        format!("/{}", cmd.name)
    }
}

fn push_clipped_span(
    spans: &mut Vec<Span<'static>>,
    text: &str,
    style: Style,
    remaining: &mut usize,
) {
    if *remaining == 0 || text.is_empty() {
        return;
    }
    let clipped = truncate_display_width(text, *remaining);
    if clipped.is_empty() {
        return;
    }
    *remaining = remaining.saturating_sub(UnicodeWidthStr::width(clipped.as_str()));
    spans.push(Span::styled(clipped, style));
}

fn finish_bordered_row(
    spans: &mut Vec<Span<'static>>,
    remaining: usize,
    content_style: Style,
    border_style: Style,
) {
    if remaining > 0 {
        spans.push(Span::styled(" ".repeat(remaining), content_style));
    }
    spans.push(Span::styled("  ", content_style));
    spans.push(Span::styled("\u{2503}", border_style));
}

fn truncate_display_width(text: &str, max_width: usize) -> String {
    if max_width == 0 {
        return String::new();
    }
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_string();
    }

    let ellipsis = "…";
    let ellipsis_width = UnicodeWidthStr::width(ellipsis);
    if max_width <= ellipsis_width {
        return ellipsis.to_string();
    }

    let mut out = String::new();
    let mut width = 0usize;
    for ch in text.chars() {
        let ch_width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + ch_width + ellipsis_width > max_width {
            break;
        }
        out.push(ch);
        width += ch_width;
    }
    out.push_str(ellipsis);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::events::CommandKind;

    fn line_width(line: &Line<'_>) -> usize {
        line.spans
            .iter()
            .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
            .sum()
    }

    fn plain(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
    }

    #[test]
    fn command_row_truncates_description_before_right_border() {
        let cmd = CommandSummary {
            name: "skill-creator".to_string(),
            aliases: Vec::new(),
            description: "Create or update a very long specialized workflow skill".to_string(),
            sort_weight: 0,
            kind: CommandKind::Skill,
            has_args: true,
            args_description: Some("[prompt]".to_string()),
        };
        let content_style = Style::default();
        let border_style = Style::default();
        let line = command_row_line(&cmd, 23, 20, content_style, border_style);

        assert_eq!(line_width(&line), 24);
        assert!(plain(&line).ends_with('\u{2503}'));
    }

    #[test]
    fn command_signature_includes_args_description() {
        let cmd = CommandSummary {
            name: "skill-creator".to_string(),
            aliases: Vec::new(),
            description: String::new(),
            sort_weight: 0,
            kind: CommandKind::Skill,
            has_args: true,
            args_description: Some("[prompt]".to_string()),
        };

        assert_eq!(command_signature(&cmd), "/skill-creator [prompt]");
    }

    #[test]
    fn clipped_span_handles_wide_text_within_width() {
        let clipped = truncate_display_width("很长的描述文本", 6);

        assert!(UnicodeWidthStr::width(clipped.as_str()) <= 6);
        assert!(clipped.ends_with('…'));
    }
}
