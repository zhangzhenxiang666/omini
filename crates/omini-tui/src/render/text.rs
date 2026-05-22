use crate::selection::{highlighted_line, selected_cols_for_screen_line};
use crate::state::UiState;
use crate::types::display::{DisplayMention, DisplayMessage, MentionKind};
use crate::types::message::TextBlock;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// 截断字符串到指定显示宽度，超长时末尾补 "..."。
pub(super) fn truncate_str(s: &str, max_width: usize) -> String {
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

pub(super) fn pad_display_width(s: &str, width: usize) -> String {
    let current = UnicodeWidthStr::width(s);
    if current >= width {
        s.to_string()
    } else {
        format!("{}{}", s, " ".repeat(width - current))
    }
}

pub(super) fn styled_wrapped_text(
    text_block: &TextBlock,
    content_width: usize,
    base_style: Style,
) -> Vec<Line<'static>> {
    styled_wrapped_ranges(&text_block.text, &[], content_width, base_style)
}

pub(super) fn styled_wrapped_display(
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

pub(super) fn line_width(line: &Line<'_>) -> usize {
    line.spans
        .iter()
        .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
        .sum()
}

pub(super) fn line_to_plain_text(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>()
}

pub(super) fn register_and_highlight_lines(
    state: &mut UiState,
    area: Rect,
    lines: &mut [Line<'static>],
) {
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

pub(super) fn apply_text_selection_highlight(
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
