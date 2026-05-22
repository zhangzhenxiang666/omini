use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

pub(super) struct ScrollableLine {
    pub(super) selected: bool,
    pub(super) line: Line<'static>,
}

pub(super) fn scrollable_lines(
    lines: Vec<ScrollableLine>,
    max_lines: usize,
    top_indicator: &str,
    bottom_indicator: &str,
) -> Vec<Line<'static>> {
    if max_lines == usize::MAX || lines.len() <= max_lines {
        return lines.into_iter().map(|line| line.line).collect();
    }
    if max_lines == 0 {
        return Vec::new();
    }

    let selected_line = lines.iter().position(|line| line.selected).unwrap_or(0);
    let (start, end, show_top, show_bottom) = scroll_window(lines.len(), selected_line, max_lines);
    let mut rendered = Vec::with_capacity(max_lines);
    if show_top {
        rendered.push(scroll_indicator_line(top_indicator));
    }
    rendered.extend(
        lines
            .into_iter()
            .skip(start)
            .take(end.saturating_sub(start))
            .map(|line| line.line),
    );
    if show_bottom {
        rendered.push(scroll_indicator_line(bottom_indicator));
    }
    rendered
}

fn scroll_window(
    total_lines: usize,
    selected_line: usize,
    max_lines: usize,
) -> (usize, usize, bool, bool) {
    if total_lines <= max_lines {
        return (0, total_lines, false, false);
    }

    if max_lines <= 2 {
        let start = selected_line
            .saturating_sub(max_lines.saturating_sub(1))
            .min(total_lines.saturating_sub(max_lines));
        return (start, start + max_lines, false, false);
    }

    let visible_lines = max_lines - 2;
    let start = selected_line
        .saturating_sub(visible_lines / 2)
        .min(total_lines.saturating_sub(visible_lines));
    let end = start + visible_lines;
    (start, end, start > 0, end < total_lines)
}

fn scroll_indicator_line(text: &str) -> Line<'static> {
    Line::from(Span::styled(
        text.to_string(),
        Style::default().fg(Color::Rgb(140, 145, 155)),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::line_to_plain_text;

    #[test]
    fn scrollable_lines_keep_selected_line_visible() {
        let lines = (0..8)
            .map(|idx| ScrollableLine {
                selected: idx == 7,
                line: Line::from(format!("line {idx}")),
            })
            .collect();

        let rendered = scrollable_lines(lines, 4, "top", "bottom")
            .iter()
            .map(line_to_plain_text)
            .collect::<Vec<_>>();

        assert_eq!(rendered.first().map(String::as_str), Some("top"));
        assert!(rendered.iter().any(|line| line == "line 7"));
        assert!(!rendered.iter().any(|line| line == "bottom"));
    }
}
