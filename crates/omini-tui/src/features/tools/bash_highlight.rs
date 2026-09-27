use ratatui::style::{Color, Style};
use ratatui::text::Span;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub const COMMAND_TEXT_FG: Color = crate::ui::theme::TEXT;
pub const PROMPT_FG: Color = crate::ui::theme::RUNNING;
const MAIN_COMMAND_FG: Color = crate::ui::theme::RUNNING;
const FLAG_FG: Color = crate::ui::theme::ERROR;
const STRING_FG: Color = crate::ui::theme::SUCCESS;

use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::OnceLock;
use tree_sitter::{Parser, Query, QueryCursor, StreamingIterator};

#[derive(Clone, Copy)]
struct Highlight {
    start: usize,
    end: usize,
    color: Color,
}

struct BashHighlighter {
    parser: Parser,
    cache: VecDeque<(String, Vec<Highlight>)>,
}

thread_local! {
    static HIGHLIGHTER: RefCell<BashHighlighter> = RefCell::new(BashHighlighter {
        parser: Parser::new(),
        cache: VecDeque::new(),
    });
}

/// 使用语法包自带的查询区分命令、字符串、变量和控制语法。
/// 查询产生 UTF-8 字节范围；范围外的原文保持原样，包括空白和未完成语法。
fn highlights(command: &str) -> Vec<Highlight> {
    static QUERY: OnceLock<Query> = OnceLock::new();
    let language = tree_sitter_bash::LANGUAGE.into();
    let query = QUERY.get_or_init(|| {
        Query::new(&language, tree_sitter_bash::HIGHLIGHT_QUERY)
            .expect("Bash 内置高亮查询应与语法版本匹配")
    });
    HIGHLIGHTER.with(|cell| {
        let mut highlighter = cell.borrow_mut();
        if let Some((_, spans)) = highlighter
            .cache
            .iter()
            .find(|(source, _)| source == command)
        {
            return spans.clone();
        }
        if highlighter.parser.set_language(&language).is_err() {
            return Vec::new();
        }
        let Some(tree) = highlighter.parser.parse(command, None) else {
            return Vec::new();
        };
        let mut cursor = QueryCursor::new();
        let mut captures = cursor.captures(query, tree.root_node(), command.as_bytes());
        let mut spans = Vec::new();
        while let Some((matched, index)) = captures.next() {
            let capture = matched.captures[*index];
            let color = match query.capture_names()[capture.index as usize] {
                "function" => MAIN_COMMAND_FG,
                "constant" => FLAG_FG,
                "string" => STRING_FG,
                "property" | "keyword" => crate::ui::theme::ACCENT,
                "comment" => crate::ui::theme::MUTED,
                "number" => crate::ui::theme::SECONDARY,
                _ => continue,
            };
            spans.push(Highlight {
                start: capture.node.start_byte(),
                end: capture.node.end_byte(),
                color,
            });
        }
        if highlighter.cache.len() == 64 {
            highlighter.cache.pop_front();
        }
        highlighter
            .cache
            .push_back((command.to_string(), spans.clone()));
        spans
    })
}

pub fn command_spans(command: &str, base_style: Style) -> Vec<Span<'static>> {
    style_tokens(command, base_style)
}

pub fn truncated_command_spans(
    command: &str,
    max_width: usize,
    base_style: Style,
) -> Vec<Span<'static>> {
    let spans = command_spans(command, base_style);
    truncate_spans(spans, max_width, base_style)
}

pub fn wrapped_command_spans(
    command: &str,
    max_width: usize,
    base_style: Style,
) -> Vec<Vec<Span<'static>>> {
    wrap_spans(command_spans(command, base_style), max_width)
}

fn style_tokens(command: &str, base_style: Style) -> Vec<Span<'static>> {
    let highlights = highlights(command);
    let mut boundaries = std::collections::BTreeSet::from([0, command.len()]);
    for highlight in &highlights {
        boundaries.insert(highlight.start);
        boundaries.insert(highlight.end);
    }
    let boundaries = boundaries.into_iter().collect::<Vec<_>>();
    let mut spans = Vec::new();
    for window in boundaries.windows(2) {
        let (start, end) = (window[0], window[1]);
        if start == end {
            continue;
        }
        // 嵌套捕获选择最具体的范围，让引号内的变量和命令替换仍能区分。
        let style = highlights
            .iter()
            .filter(|highlight| highlight.start <= start && highlight.end >= end)
            .min_by_key(|highlight| highlight.end - highlight.start)
            .map(|highlight| base_style.fg(highlight.color))
            .unwrap_or(base_style);
        spans.push(Span::styled(command[start..end].to_string(), style));
    }
    spans
}

fn truncate_spans(
    spans: Vec<Span<'static>>,
    max_width: usize,
    base_style: Style,
) -> Vec<Span<'static>> {
    let width: usize = spans
        .iter()
        .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
        .sum();
    if width <= max_width {
        return spans;
    }
    if max_width == 0 {
        return Vec::new();
    }
    let ellipsis = "...";
    let ellipsis_width = UnicodeWidthStr::width(ellipsis);
    if max_width <= ellipsis_width {
        return vec![Span::styled(
            ellipsis.chars().take(max_width).collect::<String>(),
            base_style,
        )];
    }

    let target = max_width - ellipsis_width;
    let mut out = Vec::new();
    let mut used = 0usize;
    for span in spans {
        let mut text = String::new();
        for ch in span.content.chars() {
            let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
            if used + ch_width > target {
                break;
            }
            text.push(ch);
            used += ch_width;
        }
        if !text.is_empty() {
            out.push(Span::styled(text, span.style));
        }
        if used >= target {
            break;
        }
    }
    out.push(Span::styled(ellipsis, base_style));
    out
}

fn wrap_spans(spans: Vec<Span<'static>>, max_width: usize) -> Vec<Vec<Span<'static>>> {
    let max_width = max_width.max(1);
    let mut lines: Vec<Vec<Span<'static>>> = Vec::new();
    let mut current: Vec<Span<'static>> = Vec::new();
    let mut current_width = 0usize;

    for span in spans {
        let style = span.style;
        let mut text = String::new();
        for ch in span.content.chars() {
            if ch == '\n' {
                push_span_if_not_empty(&mut current, &mut text, style);
                lines.push(current);
                current = Vec::new();
                current_width = 0;
                continue;
            }

            let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
            if current_width > 0 && current_width + ch_width > max_width {
                push_span_if_not_empty(&mut current, &mut text, style);
                lines.push(current);
                current = Vec::new();
                current_width = 0;
            }
            text.push(ch);
            current_width += ch_width;
        }
        push_span_if_not_empty(&mut current, &mut text, style);
    }

    lines.push(current);
    lines
}

fn push_span_if_not_empty(spans: &mut Vec<Span<'static>>, text: &mut String, style: Style) {
    if !text.is_empty() {
        spans.push(Span::styled(std::mem::take(text), style));
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    fn plain(spans: &[Span<'_>]) -> String {
        spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
    }

    fn has_exact_fg(spans: &[Span<'_>], text: &str, color: Color) -> bool {
        spans
            .iter()
            .any(|span| span.content.contains(text) && span.style.fg == Some(color))
    }

    #[test]
    fn highlights_command_parts_without_changing_text() {
        let command = "FOO=bar cargo test -p omini-tui 'quoted value' | rg src/main.rs";
        let spans = command_spans(command, Style::default().fg(COMMAND_TEXT_FG));

        assert_eq!(plain(&spans), command);
        assert!(has_exact_fg(&spans, "FOO", crate::ui::theme::ACCENT));
        assert!(has_exact_fg(&spans, "cargo", MAIN_COMMAND_FG));
        assert!(has_exact_fg(&spans, "test", COMMAND_TEXT_FG));
        assert!(has_exact_fg(&spans, "-p", FLAG_FG));
        assert!(has_exact_fg(&spans, "omini-tui", COMMAND_TEXT_FG));
        assert!(has_exact_fg(&spans, "'quoted value'", STRING_FG));
        assert!(has_exact_fg(&spans, "|", COMMAND_TEXT_FG));
        assert!(has_exact_fg(&spans, "rg", MAIN_COMMAND_FG));
        assert!(has_exact_fg(&spans, "src/main.rs", COMMAND_TEXT_FG));
    }

    #[test]
    fn wrapped_spans_preserve_command_text() {
        let command =
            "cargo test -p omini-tui permission_drawer_with_a_very_long_filter -- --nocapture";
        let lines = wrapped_command_spans(command, 18, Style::default());
        let rendered = lines.iter().map(|line| plain(line)).collect::<String>();

        assert!(lines.len() > 1);
        assert_eq!(rendered, command);
    }
}

#[cfg(test)]
mod syntax_tests {
    use super::*;

    #[test]
    fn preserves_nested_and_incomplete_syntax() {
        // 给定嵌套命令、中文、heredoc 和未闭合引号，语法错误也不能丢失文本。
        for source in [
            "echo \"中文 $(printf '%s' \"$HOME\")\" # 注释",
            "for item in a b; do printf '%s' \"$item\"; done",
            "cat <<'EOF'\n$(not_a_command)\nEOF\n",
            "echo \"未完成 $(date",
        ] {
            let spans = command_spans(source, Style::default());
            assert_eq!(
                spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>(),
                source
            );
        }
    }

    #[test]
    fn highlights_nested_commands_and_comments() {
        let spans = command_spans("echo $(date) # note", Style::default());
        assert!(
            spans
                .iter()
                .any(|span| span.content == "date" && span.style.fg == Some(MAIN_COMMAND_FG))
        );
        assert!(
            spans
                .iter()
                .any(|span| span.content == "# note"
                    && span.style.fg == Some(crate::ui::theme::MUTED))
        );
    }
}
