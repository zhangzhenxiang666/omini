use ratatui::style::{Color, Style};
use ratatui::text::Span;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub(crate) const COMMAND_TEXT_FG: Color = Color::Rgb(0xcd, 0xd6, 0xf4);
pub(crate) const PROMPT_FG: Color = Color::Rgb(0x8f, 0xa1, 0xb7);
const MAIN_COMMAND_FG: Color = Color::Rgb(0x89, 0xb4, 0xfa);
const FLAG_FG: Color = Color::Rgb(0xeb, 0xa0, 0xaa);
const STRING_FG: Color = Color::Rgb(0xa5, 0xe3, 0xa1);

#[derive(Clone, Copy, PartialEq, Eq)]
enum TokenKind {
    Whitespace,
    Word,
    String,
    Operator,
}

struct Token<'a> {
    text: &'a str,
    kind: TokenKind,
}

pub(crate) fn command_spans(command: &str, base_style: Style) -> Vec<Span<'static>> {
    style_tokens(command, base_style)
}

pub(crate) fn truncated_command_spans(
    command: &str,
    max_width: usize,
    base_style: Style,
) -> Vec<Span<'static>> {
    let spans = command_spans(command, base_style);
    truncate_spans(spans, max_width, base_style)
}

pub(crate) fn wrapped_command_spans(
    command: &str,
    max_width: usize,
    base_style: Style,
) -> Vec<Vec<Span<'static>>> {
    wrap_spans(command_spans(command, base_style), max_width)
}

fn style_tokens(command: &str, base_style: Style) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut expect_command = true;

    for token in tokenize(command) {
        let style = match token.kind {
            TokenKind::String => base_style.fg(STRING_FG),
            TokenKind::Whitespace => base_style,
            TokenKind::Operator => base_style,
            TokenKind::Word if is_flag(token.text) => {
                expect_command = false;
                base_style.fg(FLAG_FG)
            }
            TokenKind::Word if expect_command && is_assignment_word(token.text) => base_style,
            TokenKind::Word if expect_command => {
                expect_command = false;
                base_style.fg(MAIN_COMMAND_FG)
            }
            TokenKind::Word => base_style,
        };
        if (token.kind == TokenKind::Operator && starts_command_segment(token.text))
            || (token.kind == TokenKind::Whitespace && token.text.contains('\n'))
        {
            expect_command = true;
        }
        spans.push(Span::styled(token.text.to_string(), style));
    }

    spans
}

fn tokenize(command: &str) -> Vec<Token<'_>> {
    let mut tokens = Vec::new();
    let mut idx = 0usize;
    while idx < command.len() {
        let Some((ch, next_idx)) = next_char_at(command, idx) else {
            break;
        };

        if ch.is_whitespace() {
            let start = idx;
            idx = next_idx;
            while let Some((next, after_next)) = next_char_at(command, idx) {
                if !next.is_whitespace() {
                    break;
                }
                idx = after_next;
            }
            tokens.push(Token {
                text: &command[start..idx],
                kind: TokenKind::Whitespace,
            });
            continue;
        }

        if matches!(ch, '\'' | '"' | '`') {
            let end = quoted_end(command, idx, ch);
            tokens.push(Token {
                text: &command[idx..end],
                kind: TokenKind::String,
            });
            idx = end;
            continue;
        }

        if is_operator_char(ch) {
            let end = operator_end(command, idx, ch);
            tokens.push(Token {
                text: &command[idx..end],
                kind: TokenKind::Operator,
            });
            idx = end;
            continue;
        }

        let start = idx;
        idx = next_idx;
        while let Some((next, after_next)) = next_char_at(command, idx) {
            if next.is_whitespace() || matches!(next, '\'' | '"' | '`') || is_operator_char(next) {
                break;
            }
            if next == '\\' {
                idx = after_next;
                if let Some((_, after_escaped)) = next_char_at(command, idx) {
                    idx = after_escaped;
                }
                continue;
            }
            idx = after_next;
        }
        tokens.push(Token {
            text: &command[start..idx],
            kind: TokenKind::Word,
        });
    }
    tokens
}

fn quoted_end(command: &str, start: usize, quote: char) -> usize {
    let mut idx = start + quote.len_utf8();
    while let Some((ch, next_idx)) = next_char_at(command, idx) {
        idx = next_idx;
        if ch == '\\' && quote != '\'' {
            if let Some((_, after_escaped)) = next_char_at(command, idx) {
                idx = after_escaped;
            }
            continue;
        }
        if ch == quote {
            break;
        }
    }
    idx
}

fn operator_end(command: &str, start: usize, ch: char) -> usize {
    let next = start + ch.len_utf8();
    if let Some((after, after_idx)) = next_char_at(command, next)
        && matches!(
            (ch, after),
            ('&', '&') | ('|', '|') | ('>', '>') | ('<', '<')
        )
    {
        return after_idx;
    }
    next
}

fn is_operator_char(ch: char) -> bool {
    matches!(ch, '|' | '&' | ';' | '<' | '>')
}

fn starts_command_segment(operator: &str) -> bool {
    matches!(operator, "|" | "||" | "&&" | ";")
}

fn is_flag(word: &str) -> bool {
    word.starts_with('-') && word.len() > 1
}

fn is_assignment_word(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && name
            .chars()
            .all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
        && name
            .chars()
            .next()
            .is_some_and(|ch| ch == '_' || ch.is_ascii_alphabetic())
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

fn next_char_at(text: &str, idx: usize) -> Option<(char, usize)> {
    let ch = text.get(idx..)?.chars().next()?;
    Some((ch, idx + ch.len_utf8()))
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
            .any(|span| span.content.as_ref() == text && span.style.fg == Some(color))
    }

    #[test]
    fn highlights_command_parts_without_changing_text() {
        let command = "FOO=bar cargo test -p omini-tui 'quoted value' | rg src/main.rs";
        let spans = command_spans(command, Style::default().fg(COMMAND_TEXT_FG));

        assert_eq!(plain(&spans), command);
        assert!(has_exact_fg(&spans, "FOO=bar", COMMAND_TEXT_FG));
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
