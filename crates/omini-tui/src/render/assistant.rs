use super::INPUT_BG;
use crate::markdown::build_markdown_lines;
use crate::types::proposed_plan::{ProposedPlanParser, ProposedPlanSegment};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub(super) fn build_assistant_text_lines(text: &str, content_width: usize) -> Vec<Line<'static>> {
    let mut parser = ProposedPlanParser::new();
    let mut normal_text = String::new();
    let mut plan = String::new();
    let mut collecting_plan = false;
    let mut lines = Vec::new();

    for segment in parser.push_str(text).into_iter().chain(parser.finish()) {
        match segment {
            ProposedPlanSegment::Normal(text) => {
                normal_text.push_str(&text);
            }
            ProposedPlanSegment::ProposedPlanStart => {
                flush_assistant_markdown(&mut lines, &mut normal_text, content_width);
                plan.clear();
                collecting_plan = true;
            }
            ProposedPlanSegment::ProposedPlanDelta(delta) => {
                if collecting_plan {
                    plan.push_str(&delta);
                }
            }
            ProposedPlanSegment::ProposedPlanEnd => {
                if collecting_plan && !plan.trim().is_empty() {
                    append_assistant_lines(
                        &mut lines,
                        build_proposed_plan_lines(&plan, content_width),
                    );
                }
                plan.clear();
                collecting_plan = false;
            }
        }
    }

    flush_assistant_markdown(&mut lines, &mut normal_text, content_width);
    if collecting_plan && !plan.trim().is_empty() {
        append_assistant_lines(&mut lines, build_proposed_plan_lines(&plan, content_width));
    }

    lines
}

fn flush_assistant_markdown(
    lines: &mut Vec<Line<'static>>,
    text: &mut String,
    content_width: usize,
) {
    if text.is_empty() {
        return;
    }
    append_assistant_lines(lines, build_markdown_lines(text, content_width));
    text.clear();
}

fn append_assistant_lines(lines: &mut Vec<Line<'static>>, segment: Vec<Line<'static>>) {
    if segment.is_empty() {
        return;
    }
    if !lines.is_empty() {
        lines.push(Line::from(""));
    }
    lines.extend(segment);
}

pub(super) fn build_proposed_plan_lines(text: &str, content_width: usize) -> Vec<Line<'static>> {
    build_markdown_panel_lines("• Proposed Plan", text, content_width)
}

pub(super) fn build_llm_summary_lines(text: &str, content_width: usize) -> Vec<Line<'static>> {
    let width = content_width.max(1);
    let body = text.trim_matches('\n');
    let loading = body.trim().is_empty();
    let mut lines = vec![compact_summary_divider_line(width, loading)];
    if loading {
        return lines;
    }

    let bg_style = Style::default().bg(INPUT_BG);
    let inner_width = width.saturating_sub(4).max(1);
    lines.push(Line::from(""));
    lines.push(padded_bg_line(Vec::new(), width, bg_style));

    let escaped = escape_plan_markdown_blocks(body);
    let markdown_lines = build_markdown_lines(&escaped, inner_width);
    for line in markdown_lines {
        lines.push(padded_plan_markdown_line(line, width, bg_style));
    }
    lines.push(padded_bg_line(Vec::new(), width, bg_style));
    lines
}

fn build_markdown_panel_lines(title: &str, text: &str, content_width: usize) -> Vec<Line<'static>> {
    let width = content_width.max(1);
    let bg_style = Style::default().bg(INPUT_BG);
    let inner_width = width.saturating_sub(4).max(1);
    let mut lines = vec![
        Line::from(Span::styled(
            title.to_string(),
            Style::default().fg(Color::Rgb(0xa5, 0xac, 0xb6)),
        )),
        Line::from(""),
        padded_bg_line(Vec::new(), width, bg_style),
    ];

    let body = text.trim_matches('\n');
    if body.trim().is_empty() {
        return lines;
    }

    let escaped = escape_plan_markdown_blocks(body);
    let markdown_lines = build_markdown_lines(&escaped, inner_width);
    for line in markdown_lines {
        lines.push(padded_plan_markdown_line(line, width, bg_style));
    }
    lines.push(padded_bg_line(Vec::new(), width, bg_style));
    lines
}

fn compact_summary_divider_line(content_width: usize, loading: bool) -> Line<'static> {
    let width = content_width.max(1);
    let line_style = Style::default().fg(Color::Rgb(0x5a, 0x66, 0x76));
    let icon_style = Style::default().fg(Color::Rgb(0xc8, 0xa9, 0xee));
    let title_style = Style::default().fg(Color::Rgb(0xa5, 0xac, 0xb6));
    let title = "Compact Summary";
    let title_width = UnicodeWidthStr::width(title);
    let label_width = title_width + 4;
    let title_spans = compact_summary_title_spans(title, title_width, loading, title_style);

    if width <= label_width {
        let mut spans = vec![Span::styled("◆", icon_style)];
        if width > 1 {
            spans.push(Span::styled(" ", line_style));
            spans.extend(compact_summary_title_spans(
                title,
                width.saturating_sub(2),
                loading,
                title_style,
            ));
        }
        return Line::from(spans);
    }

    let left_width = (width - label_width) / 2;
    let right_width = width - label_width - left_width;
    let mut spans = Vec::new();
    if left_width > 0 {
        spans.push(Span::styled("─".repeat(left_width), line_style));
    }
    spans.push(Span::styled(" ", line_style));
    spans.push(Span::styled("◆", icon_style));
    spans.push(Span::styled(" ", line_style));
    spans.extend(title_spans);
    spans.push(Span::styled(" ", line_style));
    if right_width > 0 {
        spans.push(Span::styled("─".repeat(right_width), line_style));
    }
    Line::from(spans)
}

fn compact_summary_title_spans(
    title: &str,
    max_width: usize,
    loading: bool,
    title_style: Style,
) -> Vec<Span<'static>> {
    let spans = if loading {
        super::status::animated_status_spans_with_palette(
            title,
            Color::Rgb(0xc8, 0xa9, 0xee),
            Color::Rgb(0x55, 0x47, 0x65),
        )
    } else {
        vec![Span::styled(title.to_string(), title_style)]
    };
    truncate_spans_to_width(spans, max_width)
}

fn truncate_spans_to_width(spans: Vec<Span<'static>>, max_width: usize) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    let mut used = 0usize;
    for span in spans {
        let style = span.style;
        for ch in span.content.chars() {
            let width = UnicodeWidthChar::width(ch).unwrap_or(0);
            if used + width > max_width {
                return out;
            }
            used += width;
            out.push(Span::styled(ch.to_string(), style));
        }
    }
    out
}

fn escape_plan_markdown_blocks(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut fence: Option<(char, usize)> = None;

    for (idx, line) in text.split('\n').enumerate() {
        if idx > 0 {
            out.push('\n');
        }

        if let Some((fence_char, fence_len)) = fence {
            out.push_str(line);
            if let Some((candidate_char, candidate_len)) = code_fence_marker(line)
                && candidate_char == fence_char
                && candidate_len >= fence_len
            {
                fence = None;
            }
            continue;
        }

        if let Some(marker) = code_fence_marker(line) {
            fence = Some(marker);
            out.push_str(line);
        } else {
            out.push_str(&escape_plan_block_markers(line));
        }
    }

    out
}

fn code_fence_marker(line: &str) -> Option<(char, usize)> {
    let marker_idx = leading_space_bytes(line);
    if marker_idx > 3 {
        return None;
    }

    let rest = &line[marker_idx..];
    let marker = rest.chars().next()?;
    if !matches!(marker, '`' | '~') {
        return None;
    }

    let len = rest.chars().take_while(|ch| *ch == marker).count();
    (len >= 3).then_some((marker, len))
}

fn escape_plan_block_markers(line: &str) -> String {
    let marker_idx = leading_space_bytes(line);
    if marker_idx > 3 {
        return line.to_string();
    }

    let rest = &line[marker_idx..];
    if !starts_plan_block_marker(rest) {
        return line.to_string();
    }

    let mut escaped = String::with_capacity(line.len() + 1);
    escaped.push_str(&line[..marker_idx]);
    escaped.push('\\');
    escaped.push_str(rest);
    escaped
}

fn leading_space_bytes(line: &str) -> usize {
    line.as_bytes()
        .iter()
        .take_while(|byte| **byte == b' ')
        .count()
}

fn starts_plan_block_marker(rest: &str) -> bool {
    starts_heading_marker(rest)
        || starts_unordered_list_marker(rest)
        || is_thematic_break_marker(rest)
}

fn starts_heading_marker(rest: &str) -> bool {
    let marker_len = rest.chars().take_while(|ch| *ch == '#').count();
    marker_len > 0
        && marker_len <= 6
        && rest[marker_len..]
            .chars()
            .next()
            .is_none_or(char::is_whitespace)
}

fn starts_unordered_list_marker(rest: &str) -> bool {
    let Some(marker) = rest.chars().next() else {
        return false;
    };
    matches!(marker, '-' | '+' | '*')
        && rest[marker.len_utf8()..]
            .chars()
            .next()
            .is_some_and(char::is_whitespace)
}

fn is_thematic_break_marker(rest: &str) -> bool {
    let mut marker = None;
    let mut marker_count = 0usize;
    for ch in rest.chars().filter(|ch| !ch.is_whitespace()) {
        if !matches!(ch, '-' | '_' | '*') {
            return false;
        }
        if marker.is_none() {
            marker = Some(ch);
        }
        if marker != Some(ch) {
            return false;
        }
        marker_count += 1;
    }
    marker_count >= 3
}

fn padded_plan_markdown_line(
    line: Line<'static>,
    content_width: usize,
    bg_style: Style,
) -> Line<'static> {
    let left_pad = content_width.saturating_sub(1).min(2);
    let mut out = if left_pad == 0 {
        Vec::new()
    } else {
        vec![Span::styled(" ".repeat(left_pad), bg_style)]
    };
    out.extend(line.spans.into_iter().map(|span| {
        let mut span = span;
        span.style = span.style.patch(bg_style);
        span
    }));
    let used = out
        .iter()
        .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
        .sum::<usize>();
    if used < content_width {
        out.push(Span::styled(" ".repeat(content_width - used), bg_style));
    }
    Line::from(out).style(bg_style)
}

fn padded_bg_line(
    mut spans: Vec<Span<'static>>,
    content_width: usize,
    bg_style: Style,
) -> Line<'static> {
    let width = content_width.max(1);
    let used = spans
        .iter()
        .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
        .sum::<usize>();
    if used < width {
        spans.push(Span::styled(" ".repeat(width - used), bg_style));
    }
    Line::from(spans).style(bg_style)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::line_to_plain_text;
    use ratatui::style::Modifier;

    #[test]
    fn proposed_plan_preserves_markdown_text_and_highlights_inline_code() {
        let lines = build_proposed_plan_lines(
            concat!(
                "# Plan\n\n",
                "- Run `cargo test` with **bold**, *italic*, ~~old~~, [docs](https://example.com)\n",
                "---\n\n",
                "```rust\n",
                "fn main() {\n",
                "    println!(\"hi\");\n",
                "}\n",
                "```\n\n",
                "| File | Action |\n",
                "| --- | --- |\n",
                "| crates/omini-tui/src/render.rs | edit |\n\n",
                "1. Keep ordered lists as Markdown",
            ),
            80,
        );
        let rendered = lines.iter().map(line_to_plain_text).collect::<Vec<_>>();

        assert!(rendered.iter().any(|line| line.contains("# Plan")));
        assert!(
            rendered
                .iter()
                .any(|line| line.contains("  - Run cargo test with bold, italic, old, docs"))
        );
        assert!(rendered.iter().any(|line| line.contains("  ---")));
        assert!(rendered.iter().any(|line| line.contains("fn main()")));
        assert!(rendered.iter().any(|line| line.contains("println!")));
        assert!(rendered.iter().any(|line| line.contains("┌")));
        assert!(rendered.iter().any(|line| line.contains("│ File")));
        assert!(
            rendered
                .iter()
                .any(|line| line.contains("  1. Keep ordered lists as Markdown"))
        );
        assert_eq!(
            rendered.first().map(String::as_str),
            Some("• Proposed Plan")
        );
        assert_eq!(rendered.get(1).map(String::as_str), Some(""));
        assert_eq!(lines.first().and_then(|line| line.style.bg), None);
        assert_eq!(lines.get(1).and_then(|line| line.style.bg), None);
        assert!(
            lines
                .iter()
                .skip(2)
                .all(|line| line.style.bg == Some(INPUT_BG))
        );
        assert!(
            lines
                .iter()
                .flat_map(|line| line.spans.iter())
                .any(|span| span.content.as_ref() == "cargo test"
                    && span.style.fg == Some(Color::Rgb(0x42, 0xd9, 0xe8)))
        );
        assert!(
            lines
                .iter()
                .flat_map(|line| line.spans.iter())
                .any(|span| span.content.as_ref() == "bold"
                    && span.style.add_modifier.contains(Modifier::BOLD))
        );
        assert!(
            lines
                .iter()
                .flat_map(|line| line.spans.iter())
                .any(|span| span.content.as_ref() == "italic"
                    && span.style.add_modifier.contains(Modifier::ITALIC))
        );
        assert!(
            lines
                .iter()
                .flat_map(|line| line.spans.iter())
                .any(|span| span.content.as_ref() == "old"
                    && span.style.add_modifier.contains(Modifier::CROSSED_OUT))
        );
        assert!(
            lines
                .iter()
                .flat_map(|line| line.spans.iter())
                .any(|span| span.content.as_ref() == "docs"
                    && span.style.add_modifier.contains(Modifier::UNDERLINED))
        );
        assert!(!rendered.iter().any(|line| line.contains('`')));
        assert!(!rendered.iter().any(|line| line.contains("```")));
        assert!(!rendered.iter().any(|line| line.contains("\\1.")));
        assert!(!rendered.iter().any(|line| line.contains("**")));
        assert!(!rendered.iter().any(|line| line.contains("~~")));
        assert!(
            !rendered
                .iter()
                .any(|line| line.contains("https://example.com"))
        );
    }

    #[test]
    fn assistant_text_renders_embedded_plan_blocks_for_restored_sessions() {
        let lines = build_assistant_text_lines(
            "Intro\n<proposed_plan>\n# Plan\n\n- Run **tests**\n---\n</proposed_plan>\nOutro",
            80,
        );
        let rendered = lines.iter().map(line_to_plain_text).collect::<Vec<_>>();

        assert!(rendered.iter().any(|line| line == "Intro"));
        assert!(rendered.iter().any(|line| line == "Outro"));
        assert!(rendered.iter().any(|line| line.contains("• Proposed Plan")));
        assert!(rendered.iter().any(|line| line.contains("  # Plan")));
        assert!(rendered.iter().any(|line| line.contains("  - Run tests")));
        assert!(rendered.iter().any(|line| line.contains("  ---")));
        assert!(
            lines
                .iter()
                .flat_map(|line| line.spans.iter())
                .any(|span| span.content.as_ref() == "tests"
                    && span.style.add_modifier.contains(Modifier::BOLD))
        );
        assert!(!rendered.iter().any(|line| line.contains("<proposed_plan>")));
        assert!(!rendered.iter().any(|line| line.contains("**")));
    }

    #[test]
    fn llm_summary_uses_compact_divider_and_body_panel() {
        let lines = build_llm_summary_lines("# Summary\n\n- Preserve **context**", 80);
        let rendered = lines.iter().map(line_to_plain_text).collect::<Vec<_>>();

        assert!(
            rendered
                .first()
                .is_some_and(|line| line.contains("◆ Compact Summary")),
            "{rendered:?}"
        );
        assert!(rendered.iter().any(|line| line.contains("  # Summary")));
        assert!(
            rendered
                .iter()
                .any(|line| line.contains("  - Preserve context"))
        );
        assert_eq!(lines.first().and_then(|line| line.style.bg), None);
        assert_eq!(lines.get(1).and_then(|line| line.style.bg), None);
        assert!(
            lines
                .iter()
                .skip(2)
                .all(|line| line.style.bg == Some(INPUT_BG))
        );
    }

    #[test]
    fn empty_llm_summary_renders_loading_divider_without_body_panel() {
        let lines = build_llm_summary_lines("", 80);
        let rendered = lines.iter().map(line_to_plain_text).collect::<Vec<_>>();

        assert_eq!(lines.len(), 1);
        assert!(
            rendered
                .first()
                .is_some_and(|line| line.contains("◆ Compact Summary")),
            "{rendered:?}"
        );
        assert!(lines.iter().all(|line| line.style.bg != Some(INPUT_BG)));
    }

    #[test]
    fn assistant_text_keeps_markdown_tables_intact() {
        let lines = build_assistant_text_lines(
            "Results:\n\n| Name | Count |\n| --- | ---: |\n| apple | 12 |\n| banana | 3 |",
            80,
        );
        let rendered = lines.iter().map(line_to_plain_text).collect::<Vec<_>>();

        assert!(rendered.iter().any(|line| line == "Results:"));
        assert!(rendered.iter().any(|line| line.contains("┌")));
        assert!(rendered.iter().any(|line| line.contains("│ Name")));
        assert!(rendered.iter().any(|line| line.contains("│ apple")));
        assert!(rendered.iter().any(|line| line.contains("│ banana")));
        assert!(!rendered.iter().any(|line| line.contains("| Name |")));
        assert!(!rendered.iter().any(|line| line.contains("| --- |")));
    }
}
