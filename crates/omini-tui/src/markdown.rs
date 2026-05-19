use pulldown_cmark::{Alignment, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const DIM: Color = Color::Rgb(0x8a, 0x91, 0x9b);
const INLINE_CODE_FG: Color = Color::Rgb(0x42, 0xd9, 0xe8);
const LINK: Color = Color::Rgb(0x42, 0xd9, 0xe8);
const TABLE_BORDER: Color = Color::Rgb(0x5a, 0x66, 0x76);
const TEXT_PATH_FG: Color = Color::Rgb(0x78, 0xbd, 0xc7);
const TEXT_COMMAND_FG: Color = Color::Rgb(0x8f, 0xb3, 0xd1);
const TEXT_FLAG_FG: Color = Color::Rgb(0x9a, 0xa3, 0xad);
const TEXT_REF_FG: Color = Color::Rgb(0xc6, 0xa7, 0x7b);
const CODE_KEYWORD_FG: Color = Color::Rgb(0xb8, 0xa5, 0xd6);
const CODE_TYPE_FG: Color = Color::Rgb(0x8f, 0xc6, 0xb2);
const CODE_FUNCTION_FG: Color = Color::Rgb(0xa9, 0xbf, 0xdc);
const CODE_STRING_FG: Color = Color::Rgb(0xc8, 0xb8, 0x85);
const CODE_COMMENT_FG: Color = Color::Rgb(0x7f, 0x87, 0x92);
const CODE_NUMBER_FG: Color = Color::Rgb(0xc1, 0xa3, 0x83);
const CODE_ATTR_FG: Color = Color::Rgb(0xb9, 0x9c, 0x78);

pub(crate) fn build_markdown_lines(text: &str, content_width: usize) -> Vec<Line<'static>> {
    if text.is_empty() {
        return Vec::new();
    }

    let options = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_HEADING_ATTRIBUTES;
    let parser = Parser::new_ext(text, options);
    let mut renderer = MarkdownRenderer::new(content_width);

    for event in parser {
        renderer.handle_event(event);
    }

    renderer.finish()
}

struct MarkdownRenderer {
    content_width: usize,
    lines: Vec<Line<'static>>,
    inline: Vec<Span<'static>>,
    style_stack: Vec<Style>,
    current_block: BlockKind,
    block_prefix: Option<BlockPrefix>,
    list_stack: Vec<ListState>,
    table: Option<TableState>,
    code_block: Option<CodeBlock>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    None,
    Paragraph,
    Heading,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BlockPrefix {
    first: String,
    continuation: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ListState {
    next_number: Option<u64>,
}

#[derive(Debug, Clone)]
struct TableState {
    alignments: Vec<Alignment>,
    header: Vec<String>,
    rows: Vec<Vec<String>>,
    current_row: Vec<String>,
    current_cell: String,
    in_head: bool,
    in_cell: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CodeBlock {
    language: CodeLanguage,
    content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodeLanguage {
    Unknown,
    Rust,
    Python,
    JavaScript,
    TypeScript,
    Go,
    Java,
    C,
    Cpp,
}

impl MarkdownRenderer {
    fn new(content_width: usize) -> Self {
        Self {
            content_width,
            lines: Vec::new(),
            inline: Vec::new(),
            style_stack: Vec::new(),
            current_block: BlockKind::None,
            block_prefix: None,
            list_stack: Vec::new(),
            table: None,
            code_block: None,
        }
    }

    fn handle_event(&mut self, event: Event<'_>) {
        if self.code_block.is_some() {
            self.handle_code_event(event);
            return;
        }

        if self.table.is_some() {
            self.handle_table_event(event);
            return;
        }

        match event {
            Event::Start(tag) => self.handle_start(tag),
            Event::End(tag) => self.handle_end(tag),
            Event::Text(text) => self.push_text(&text),
            Event::Code(text) => self.push_inline_code(&text),
            Event::Html(text) | Event::InlineHtml(text) => self.push_text(&text),
            Event::SoftBreak | Event::HardBreak => self.push_text("\n"),
            Event::Rule => self.push_rule(),
            Event::TaskListMarker(checked) => {
                self.push_text(if checked { "[x] " } else { "[ ] " });
            }
            Event::FootnoteReference(text) => self.push_text(&format!("[{text}]")),
            _ => {}
        }
    }

    fn handle_start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {
                self.current_block = BlockKind::Paragraph;
            }
            Tag::Heading { .. } => {
                self.current_block = BlockKind::Heading;
                self.style_stack.push(heading_style());
            }
            Tag::CodeBlock(kind) => {
                self.code_block = Some(CodeBlock {
                    language: code_block_language(kind),
                    content: String::new(),
                });
            }
            Tag::Table(alignments) => {
                self.table = Some(TableState {
                    alignments,
                    header: Vec::new(),
                    rows: Vec::new(),
                    current_row: Vec::new(),
                    current_cell: String::new(),
                    in_head: false,
                    in_cell: false,
                });
            }
            Tag::List(start) => {
                self.list_stack.push(ListState { next_number: start });
            }
            Tag::Item => {
                self.block_prefix = Some(self.next_list_prefix());
            }
            Tag::Emphasis => {
                self.style_stack
                    .push(Style::default().add_modifier(Modifier::ITALIC));
            }
            Tag::Strong => {
                self.style_stack
                    .push(Style::default().add_modifier(Modifier::BOLD));
            }
            Tag::Strikethrough => {
                self.style_stack
                    .push(Style::default().add_modifier(Modifier::CROSSED_OUT));
            }
            Tag::Link { .. } => {
                self.style_stack
                    .push(Style::default().fg(LINK).add_modifier(Modifier::UNDERLINED));
            }
            Tag::BlockQuote(_) => {
                self.style_stack.push(Style::default().fg(DIM));
            }
            _ => {}
        }
    }

    fn handle_end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                self.flush_inline_block();
                self.push_blank();
                self.current_block = BlockKind::None;
            }
            TagEnd::Heading(_) => {
                self.flush_inline_block();
                self.push_blank();
                self.pop_style();
                self.current_block = BlockKind::None;
            }
            TagEnd::List(_) => {
                self.list_stack.pop();
                self.push_blank();
            }
            TagEnd::Item => {
                if !self.inline.is_empty() {
                    self.flush_inline_block();
                }
                self.block_prefix = None;
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough | TagEnd::Link => {
                self.pop_style();
            }
            TagEnd::BlockQuote(_) => {
                self.block_prefix = None;
                self.pop_style();
                self.push_blank();
            }
            _ => {}
        }
    }

    fn handle_code_event(&mut self, event: Event<'_>) {
        match event {
            Event::End(TagEnd::CodeBlock) => {
                if let Some(code_block) = self.code_block.take() {
                    self.render_code_block(code_block);
                    self.push_blank();
                }
            }
            Event::Text(text) | Event::Code(text) | Event::Html(text) | Event::InlineHtml(text) => {
                if let Some(code_block) = &mut self.code_block {
                    code_block.content.push_str(&text);
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if let Some(code_block) = &mut self.code_block {
                    code_block.content.push('\n');
                }
            }
            _ => {}
        }
    }

    fn handle_table_event(&mut self, event: Event<'_>) {
        let mut should_render = false;

        if let Some(table) = &mut self.table {
            match event {
                Event::Start(Tag::TableHead) => {
                    table.in_head = true;
                    table.current_row.clear();
                }
                Event::End(TagEnd::TableHead) => {
                    table.header = std::mem::take(&mut table.current_row);
                    table.in_head = false;
                }
                Event::Start(Tag::TableRow) => {
                    table.current_row.clear();
                }
                Event::End(TagEnd::TableRow) => {
                    table.rows.push(std::mem::take(&mut table.current_row));
                }
                Event::Start(Tag::TableCell) => {
                    table.in_cell = true;
                    table.current_cell.clear();
                }
                Event::End(TagEnd::TableCell) => {
                    let cell = table.current_cell.trim().to_string();
                    table.current_row.push(cell);
                    table.current_cell.clear();
                    table.in_cell = false;
                }
                Event::Text(text) | Event::Code(text) => {
                    if table.in_cell {
                        table.current_cell.push_str(&text);
                    }
                }
                Event::SoftBreak | Event::HardBreak => {
                    if table.in_cell {
                        table.current_cell.push(' ');
                    }
                }
                Event::End(TagEnd::Table) => {
                    should_render = true;
                }
                _ => {}
            }
        }

        if should_render && let Some(table) = self.table.take() {
            self.render_table(table);
            self.push_blank();
        }
    }

    fn push_text(&mut self, text: &str) {
        let style = self.current_style();
        if style == Style::default() {
            push_plain_text_tokens(&mut self.inline, text, style);
        } else {
            push_span_text(&mut self.inline, text, style);
        }
    }

    fn push_inline_code(&mut self, text: &str) {
        let style = Style::default()
            .fg(INLINE_CODE_FG)
            .add_modifier(Modifier::BOLD);
        push_span_text(&mut self.inline, text, style);
    }

    fn push_rule(&mut self) {
        self.flush_inline_block();
        let width = self.content_width.max(1);
        self.lines.push(Line::from(Span::styled(
            "\u{2500}".repeat(width),
            Style::default().fg(TABLE_BORDER),
        )));
        self.push_blank();
    }

    fn flush_inline_block(&mut self) {
        if self.inline.is_empty() {
            return;
        }

        let spans = std::mem::take(&mut self.inline);
        let (first_prefix, continuation_prefix) = if self.current_block == BlockKind::Heading {
            (vec![Span::raw("\u{00b7} ")], vec![Span::raw("  ")])
        } else {
            self.prefix_spans(self.block_prefix.as_ref())
        };

        let mut wrapped =
            wrap_spans_with_prefix(spans, first_prefix, continuation_prefix, self.content_width);
        self.lines.append(&mut wrapped);
    }

    fn render_code_block(&mut self, code_block: CodeBlock) {
        if code_block.content.is_empty() {
            self.lines.push(Line::from(""));
            return;
        }

        let available = self.content_width.max(1);
        for raw_line in code_block.content.split('\n') {
            if raw_line.is_empty() {
                self.lines.push(Line::from(""));
                continue;
            }

            let spans = highlight_code_line(raw_line, code_block.language);
            let mut wrapped = wrap_spans_with_prefix(spans, Vec::new(), Vec::new(), available);
            self.lines.append(&mut wrapped);
        }
    }

    fn render_table(&mut self, table: TableState) {
        let column_count = table
            .header
            .len()
            .max(table.rows.iter().map(Vec::len).max().unwrap_or(0));
        if column_count == 0 {
            return;
        }

        let Some(column_count) = visible_table_column_count(column_count, self.content_width)
        else {
            return;
        };
        let widths = table_column_widths(&table, column_count, self.content_width);
        if widths.is_empty() {
            return;
        }

        self.lines.push(render_table_border(
            &widths, '\u{250c}', '\u{252c}', '\u{2510}',
        ));
        if !table.header.is_empty() {
            self.lines.extend(render_table_rows(
                &table.header,
                &widths,
                &table.alignments,
                true,
            ));
            self.lines.push(render_table_border(
                &widths, '\u{251c}', '\u{253c}', '\u{2524}',
            ));
        }

        for (idx, row) in table.rows.iter().enumerate() {
            self.lines
                .extend(render_table_rows(row, &widths, &table.alignments, false));
            if idx + 1 < table.rows.len() {
                self.lines.push(render_table_border(
                    &widths, '\u{251c}', '\u{253c}', '\u{2524}',
                ));
            }
        }
        self.lines.push(render_table_padding_row(&widths));
        self.lines.push(render_table_border(
            &widths, '\u{2514}', '\u{2534}', '\u{2518}',
        ));
    }

    fn push_blank(&mut self) {
        if self.lines.last().is_some_and(is_blank_line) {
            return;
        }
        self.lines.push(Line::from(""));
    }

    fn current_style(&self) -> Style {
        self.style_stack
            .iter()
            .copied()
            .fold(Style::default(), Style::patch)
    }

    fn pop_style(&mut self) {
        self.style_stack.pop();
    }

    fn next_list_prefix(&mut self) -> BlockPrefix {
        let depth = self.list_stack.len().saturating_sub(1);
        let indent = "  ".repeat(depth);
        let marker = if let Some(list) = self.list_stack.last_mut() {
            if let Some(next_number) = &mut list.next_number {
                let marker = format!("{next_number}. ");
                *next_number += 1;
                marker
            } else {
                "\u{2022} ".to_string()
            }
        } else {
            "\u{2022} ".to_string()
        };
        let continuation = " ".repeat(UnicodeWidthStr::width(marker.as_str()));
        BlockPrefix {
            first: format!("{indent}{marker}"),
            continuation: format!("{indent}{continuation}"),
        }
    }

    fn prefix_spans(
        &self,
        prefix: Option<&BlockPrefix>,
    ) -> (Vec<Span<'static>>, Vec<Span<'static>>) {
        let Some(prefix) = prefix else {
            return (Vec::new(), Vec::new());
        };
        let style = Style::default().fg(DIM);
        (
            vec![Span::styled(prefix.first.clone(), style)],
            vec![Span::styled(prefix.continuation.clone(), style)],
        )
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        if let Some(code_block) = self.code_block.take() {
            self.render_code_block(code_block);
        }
        if let Some(table) = self.table.take() {
            self.render_table(table);
        }
        self.flush_inline_block();

        while self.lines.last().is_some_and(is_blank_line) {
            self.lines.pop();
        }

        self.lines
    }
}

fn heading_style() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}

fn push_span_text(spans: &mut Vec<Span<'static>>, text: &str, style: Style) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = spans.last_mut()
        && last.style == style
    {
        last.content.to_mut().push_str(text);
        return;
    }
    spans.push(Span::styled(text.to_string(), style));
}

fn push_plain_text_tokens(spans: &mut Vec<Span<'static>>, text: &str, base_style: Style) {
    let mut idx = 0usize;
    while idx < text.len() {
        let Some((ch, next_idx)) = next_char_at(text, idx) else {
            break;
        };

        if ch.is_whitespace() {
            let start = idx;
            idx = next_idx;
            while let Some((next, after_next)) = next_char_at(text, idx) {
                if !next.is_whitespace() {
                    break;
                }
                idx = after_next;
            }
            push_span_text(spans, &text[start..idx], base_style);
            continue;
        }

        let start = idx;
        idx = next_idx;
        while let Some((next, after_next)) = next_char_at(text, idx) {
            if next.is_whitespace() {
                break;
            }
            idx = after_next;
        }
        push_plain_word(spans, &text[start..idx], base_style);
    }
}

fn push_plain_word(spans: &mut Vec<Span<'static>>, word: &str, base_style: Style) {
    let leading_end = leading_punctuation_end(word);
    let trailing_start = trailing_punctuation_start(&word[leading_end..]) + leading_end;

    if leading_end > 0 {
        push_span_text(spans, &word[..leading_end], base_style);
    }

    let core = &word[leading_end..trailing_start];
    if core.is_empty() {
        push_span_text(spans, &word[leading_end..], base_style);
        return;
    }

    if let Some(color) = plain_token_color(core) {
        push_span_text(spans, core, base_style.fg(color));
    } else {
        push_span_text(spans, core, base_style);
    }

    if trailing_start < word.len() {
        push_span_text(spans, &word[trailing_start..], base_style);
    }
}

fn leading_punctuation_end(word: &str) -> usize {
    let mut end = 0usize;
    for (idx, ch) in word.char_indices() {
        if matches!(ch, '"' | '\'' | '(' | '[' | '{' | '<') {
            end = idx + ch.len_utf8();
        } else {
            break;
        }
    }
    end
}

fn trailing_punctuation_start(word: &str) -> usize {
    let mut start = word.len();
    while start > 0 {
        let Some(ch) = word[..start].chars().next_back() else {
            break;
        };
        if matches!(
            ch,
            '"' | '\'' | '.' | ',' | ':' | ';' | '!' | '?' | ')' | ']' | '}' | '>'
        ) {
            start -= ch.len_utf8();
        } else {
            break;
        }
    }
    start
}

fn plain_token_color(token: &str) -> Option<Color> {
    if is_reference_token(token) {
        Some(TEXT_REF_FG)
    } else if is_flag_token(token) {
        Some(TEXT_FLAG_FG)
    } else if is_command_token(token) {
        Some(TEXT_COMMAND_FG)
    } else if is_path_token(token) {
        Some(TEXT_PATH_FG)
    } else {
        None
    }
}

fn is_reference_token(token: &str) -> bool {
    if let Some(rest) = token.strip_prefix('#') {
        return !rest.is_empty() && rest.chars().all(|ch| ch.is_ascii_digit());
    }
    if let Some(rest) = token.strip_prefix('@') {
        return !rest.is_empty()
            && rest
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/'));
    }
    false
}

fn is_flag_token(token: &str) -> bool {
    let Some(rest) = token.strip_prefix('-') else {
        return false;
    };
    if rest.is_empty() || rest.chars().next().is_some_and(|ch| ch.is_ascii_digit()) {
        return false;
    }
    rest.chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '=' | '.'))
}

fn is_command_token(token: &str) -> bool {
    matches!(
        token,
        "bun"
            | "cargo"
            | "curl"
            | "deno"
            | "git"
            | "go"
            | "java"
            | "javac"
            | "node"
            | "npm"
            | "pnpm"
            | "python"
            | "python3"
            | "rg"
            | "rustc"
            | "uv"
            | "yarn"
    )
}

fn is_path_token(token: &str) -> bool {
    if token.contains("://") {
        return false;
    }
    if token.starts_with("./")
        || token.starts_with("../")
        || token.starts_with("~/")
        || token.starts_with('/')
        || token.contains('/')
    {
        return token.chars().any(|ch| ch.is_ascii_alphanumeric());
    }

    let token = token
        .split_once(':')
        .map_or(token, |(path, _)| path)
        .trim_start_matches('.');
    let lower = token.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "cargo.lock"
            | "cargo.toml"
            | "dockerfile"
            | "go.mod"
            | "go.sum"
            | "makefile"
            | "package.json"
            | "readme.md"
            | "tsconfig.json"
    ) {
        return true;
    }

    let Some((_, ext)) = lower.rsplit_once('.') else {
        return false;
    };
    matches!(
        ext,
        "c" | "cc"
            | "cpp"
            | "cxx"
            | "go"
            | "h"
            | "hpp"
            | "java"
            | "js"
            | "json"
            | "jsx"
            | "lock"
            | "md"
            | "py"
            | "rs"
            | "toml"
            | "ts"
            | "tsx"
            | "yaml"
            | "yml"
    )
}

fn code_block_language(kind: CodeBlockKind<'_>) -> CodeLanguage {
    match kind {
        CodeBlockKind::Fenced(info) => code_language_from_info(&info),
        CodeBlockKind::Indented => CodeLanguage::Unknown,
    }
}

fn code_language_from_info(info: &str) -> CodeLanguage {
    let Some(raw) = info.split_whitespace().next() else {
        return CodeLanguage::Unknown;
    };
    let language = raw
        .trim_start_matches("{.")
        .trim_start_matches('.')
        .trim_end_matches('}')
        .to_ascii_lowercase();
    match language.as_str() {
        "rs" | "rust" => CodeLanguage::Rust,
        "py" | "python" => CodeLanguage::Python,
        "js" | "javascript" | "jsx" => CodeLanguage::JavaScript,
        "ts" | "tsx" | "typescript" => CodeLanguage::TypeScript,
        "go" | "golang" => CodeLanguage::Go,
        "java" => CodeLanguage::Java,
        "c" | "h" => CodeLanguage::C,
        "c++" | "cc" | "cpp" | "cxx" | "hpp" => CodeLanguage::Cpp,
        _ => CodeLanguage::Unknown,
    }
}

fn highlight_code_line(line: &str, language: CodeLanguage) -> Vec<Span<'static>> {
    if language == CodeLanguage::Unknown {
        return vec![Span::raw(line.to_string())];
    }

    let mut spans = Vec::new();
    let mut idx = 0usize;
    while idx < line.len() {
        if let Some(end) = comment_end(line, idx, language) {
            push_span_text(&mut spans, &line[idx..end], comment_style());
            idx = end;
            continue;
        }

        if let Some(end) = preprocessor_end(line, idx, language) {
            push_span_text(&mut spans, &line[idx..end], attr_style());
            idx = end;
            continue;
        }

        if let Some(end) = rust_lifetime_end(line, idx, language) {
            push_span_text(&mut spans, &line[idx..end], attr_style());
            idx = end;
            continue;
        }

        if let Some(end) = string_end(line, idx, language) {
            push_span_text(&mut spans, &line[idx..end], string_style());
            idx = end;
            continue;
        }

        let Some((ch, next_idx)) = next_char_at(line, idx) else {
            break;
        };

        if ch == '@' && matches!(language, CodeLanguage::Python | CodeLanguage::Java) {
            let end = decorator_end(line, idx);
            push_span_text(&mut spans, &line[idx..end], attr_style());
            idx = end;
            continue;
        }

        if ch.is_ascii_digit() {
            let end = number_end(line, idx);
            push_span_text(&mut spans, &line[idx..end], number_style());
            idx = end;
            continue;
        }

        if is_ident_start(ch, language) {
            let mut end = ident_end(line, idx, language);
            let ident = &line[idx..end];
            if language == CodeLanguage::Rust && line[end..].starts_with('!') {
                end += 1;
                push_span_text(&mut spans, &line[idx..end], function_style());
                idx = end;
                continue;
            }

            let style = code_ident_style(ident, line, end, language);
            push_span_text(&mut spans, ident, style);
            idx = end;
            continue;
        }

        push_span_text(&mut spans, &line[idx..next_idx], Style::default());
        idx = next_idx;
    }

    spans
}

fn code_ident_style(ident: &str, line: &str, end: usize, language: CodeLanguage) -> Style {
    if is_language_keyword(language, ident) {
        keyword_style()
    } else if is_language_type(language, ident) {
        type_style()
    } else if next_non_ws(line, end) == Some('(') {
        function_style()
    } else {
        Style::default()
    }
}

fn comment_end(line: &str, idx: usize, language: CodeLanguage) -> Option<usize> {
    match language {
        CodeLanguage::Python => line[idx..].starts_with('#').then_some(line.len()),
        CodeLanguage::Rust
        | CodeLanguage::JavaScript
        | CodeLanguage::TypeScript
        | CodeLanguage::Go
        | CodeLanguage::Java
        | CodeLanguage::C
        | CodeLanguage::Cpp => {
            if line[idx..].starts_with("//") {
                Some(line.len())
            } else if line[idx..].starts_with("/*") {
                Some(
                    line[idx + 2..]
                        .find("*/")
                        .map_or(line.len(), |offset| idx + 2 + offset + 2),
                )
            } else {
                None
            }
        }
        CodeLanguage::Unknown => None,
    }
}

fn preprocessor_end(line: &str, idx: usize, language: CodeLanguage) -> Option<usize> {
    if !matches!(language, CodeLanguage::C | CodeLanguage::Cpp) || !line[idx..].starts_with('#') {
        return None;
    }
    Some(line.len())
}

fn rust_lifetime_end(line: &str, idx: usize, language: CodeLanguage) -> Option<usize> {
    if language != CodeLanguage::Rust || !line[idx..].starts_with('\'') {
        return None;
    }

    let after_quote = idx + 1;
    let (next, _) = next_char_at(line, after_quote)?;
    if !is_ident_start(next, language) {
        return None;
    }

    let end = ident_end(line, after_quote, language);
    if line[end..].starts_with('\'') {
        return None;
    }
    Some(end)
}

fn string_end(line: &str, idx: usize, language: CodeLanguage) -> Option<usize> {
    if language == CodeLanguage::Python
        && (line[idx..].starts_with("\"\"\"") || line[idx..].starts_with("'''"))
    {
        let marker = &line[idx..idx + 3];
        return Some(
            line[idx + 3..]
                .find(marker)
                .map_or(line.len(), |offset| idx + 3 + offset + 3),
        );
    }

    let (quote, mut cursor) = next_char_at(line, idx)?;
    let can_backtick = matches!(
        language,
        CodeLanguage::JavaScript | CodeLanguage::TypeScript
    );
    if quote != '"' && quote != '\'' && !(can_backtick && quote == '`') {
        return None;
    }

    while cursor < line.len() {
        let Some((ch, next_idx)) = next_char_at(line, cursor) else {
            break;
        };
        cursor = next_idx;
        if ch == '\\' {
            if let Some((_, after_escape)) = next_char_at(line, cursor) {
                cursor = after_escape;
            }
            continue;
        }
        if ch == quote {
            break;
        }
    }
    Some(cursor)
}

fn decorator_end(line: &str, idx: usize) -> usize {
    let mut cursor = idx + 1;
    while let Some((ch, next_idx)) = next_char_at(line, cursor) {
        if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.') {
            cursor = next_idx;
        } else {
            break;
        }
    }
    cursor.max(idx + 1)
}

fn number_end(line: &str, idx: usize) -> usize {
    let mut cursor = idx;
    while let Some((ch, next_idx)) = next_char_at(line, cursor) {
        if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.') {
            cursor = next_idx;
        } else {
            break;
        }
    }
    cursor
}

fn ident_end(line: &str, idx: usize, language: CodeLanguage) -> usize {
    let mut cursor = idx;
    while let Some((ch, next_idx)) = next_char_at(line, cursor) {
        if is_ident_continue(ch, language) {
            cursor = next_idx;
        } else {
            break;
        }
    }
    cursor
}

fn next_non_ws(line: &str, idx: usize) -> Option<char> {
    line[idx..].chars().find(|ch| !ch.is_whitespace())
}

fn next_char_at(text: &str, idx: usize) -> Option<(char, usize)> {
    let ch = text.get(idx..)?.chars().next()?;
    Some((ch, idx + ch.len_utf8()))
}

fn is_ident_start(ch: char, language: CodeLanguage) -> bool {
    ch == '_'
        || ch.is_ascii_alphabetic()
        || (matches!(
            language,
            CodeLanguage::JavaScript | CodeLanguage::TypeScript
        ) && ch == '$')
}

fn is_ident_continue(ch: char, language: CodeLanguage) -> bool {
    is_ident_start(ch, language) || ch.is_ascii_digit()
}

fn is_language_keyword(language: CodeLanguage, ident: &str) -> bool {
    match language {
        CodeLanguage::Rust => contains_word(
            &[
                "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else",
                "enum", "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match",
                "mod", "move", "mut", "pub", "ref", "return", "self", "Self", "static", "struct",
                "super", "trait", "true", "type", "unsafe", "use", "where", "while",
            ],
            ident,
        ),
        CodeLanguage::Python => contains_word(
            &[
                "and", "as", "assert", "async", "await", "break", "class", "continue", "def",
                "del", "elif", "else", "except", "False", "finally", "for", "from", "global", "if",
                "import", "in", "is", "lambda", "None", "nonlocal", "not", "or", "pass", "raise",
                "return", "self", "True", "try", "while", "with", "yield",
            ],
            ident,
        ),
        CodeLanguage::JavaScript => contains_word(
            &[
                "async",
                "await",
                "break",
                "case",
                "catch",
                "class",
                "const",
                "continue",
                "debugger",
                "default",
                "delete",
                "do",
                "else",
                "export",
                "extends",
                "false",
                "finally",
                "for",
                "from",
                "function",
                "if",
                "import",
                "in",
                "instanceof",
                "let",
                "new",
                "null",
                "of",
                "return",
                "static",
                "super",
                "switch",
                "this",
                "throw",
                "true",
                "try",
                "typeof",
                "undefined",
                "var",
                "void",
                "while",
                "yield",
            ],
            ident,
        ),
        CodeLanguage::TypeScript => {
            is_language_keyword(CodeLanguage::JavaScript, ident)
                || contains_word(
                    &[
                        "abstract",
                        "as",
                        "declare",
                        "enum",
                        "implements",
                        "interface",
                        "keyof",
                        "namespace",
                        "private",
                        "protected",
                        "public",
                        "readonly",
                        "satisfies",
                        "type",
                    ],
                    ident,
                )
        }
        CodeLanguage::Go => contains_word(
            &[
                "break",
                "case",
                "chan",
                "const",
                "continue",
                "defer",
                "else",
                "fallthrough",
                "for",
                "func",
                "go",
                "goto",
                "if",
                "import",
                "interface",
                "map",
                "nil",
                "package",
                "range",
                "return",
                "select",
                "struct",
                "switch",
                "type",
                "var",
            ],
            ident,
        ),
        CodeLanguage::Java => contains_word(
            &[
                "abstract",
                "assert",
                "break",
                "case",
                "catch",
                "class",
                "const",
                "continue",
                "default",
                "do",
                "else",
                "enum",
                "extends",
                "false",
                "final",
                "finally",
                "for",
                "if",
                "implements",
                "import",
                "instanceof",
                "interface",
                "new",
                "null",
                "package",
                "private",
                "protected",
                "public",
                "return",
                "static",
                "super",
                "switch",
                "this",
                "throw",
                "throws",
                "true",
                "try",
                "void",
                "while",
            ],
            ident,
        ),
        CodeLanguage::C => contains_word(
            &[
                "auto", "break", "case", "const", "continue", "default", "do", "else", "enum",
                "extern", "for", "goto", "if", "inline", "register", "return", "sizeof", "static",
                "struct", "switch", "typedef", "union", "volatile", "while",
            ],
            ident,
        ),
        CodeLanguage::Cpp => {
            is_language_keyword(CodeLanguage::C, ident)
                || contains_word(
                    &[
                        "alignas",
                        "alignof",
                        "class",
                        "constexpr",
                        "decltype",
                        "delete",
                        "explicit",
                        "export",
                        "false",
                        "friend",
                        "mutable",
                        "namespace",
                        "new",
                        "noexcept",
                        "nullptr",
                        "operator",
                        "private",
                        "protected",
                        "public",
                        "template",
                        "this",
                        "throw",
                        "true",
                        "try",
                        "typename",
                        "using",
                        "virtual",
                    ],
                    ident,
                )
        }
        CodeLanguage::Unknown => false,
    }
}

fn is_language_type(language: CodeLanguage, ident: &str) -> bool {
    match language {
        CodeLanguage::Rust => contains_word(
            &[
                "bool", "Box", "char", "f32", "f64", "i16", "i32", "i64", "i8", "isize", "Option",
                "Result", "String", "str", "u16", "u32", "u64", "u8", "usize", "Vec",
            ],
            ident,
        ),
        CodeLanguage::Python => contains_word(
            &[
                "bool", "bytes", "dict", "float", "int", "list", "object", "set", "str", "tuple",
            ],
            ident,
        ),
        CodeLanguage::JavaScript => contains_word(
            &[
                "Array", "BigInt", "Boolean", "Error", "Map", "Number", "Object", "Promise", "Set",
                "String", "Symbol",
            ],
            ident,
        ),
        CodeLanguage::TypeScript => contains_word(
            &[
                "Array", "bigint", "boolean", "Error", "Map", "never", "number", "object",
                "Promise", "Record", "Set", "string", "String", "symbol", "unknown", "void",
            ],
            ident,
        ),
        CodeLanguage::Go => contains_word(
            &[
                "any",
                "bool",
                "byte",
                "complex64",
                "complex128",
                "error",
                "float32",
                "float64",
                "int",
                "int16",
                "int32",
                "int64",
                "int8",
                "rune",
                "string",
                "uint",
                "uint16",
                "uint32",
                "uint64",
                "uint8",
                "uintptr",
            ],
            ident,
        ),
        CodeLanguage::Java => contains_word(
            &[
                "boolean", "byte", "char", "double", "float", "int", "Integer", "List", "long",
                "Map", "Object", "String",
            ],
            ident,
        ),
        CodeLanguage::C => contains_word(
            &[
                "bool", "char", "double", "FILE", "float", "int", "int16_t", "int32_t", "int64_t",
                "int8_t", "long", "size_t", "ssize_t", "uint16_t", "uint32_t", "uint64_t",
                "uint8_t", "void",
            ],
            ident,
        ),
        CodeLanguage::Cpp => {
            is_language_type(CodeLanguage::C, ident)
                || contains_word(
                    &[
                        "auto",
                        "bool",
                        "optional",
                        "size_t",
                        "std",
                        "string",
                        "string_view",
                        "unique_ptr",
                        "vector",
                    ],
                    ident,
                )
        }
        CodeLanguage::Unknown => false,
    }
}

fn contains_word(words: &[&str], ident: &str) -> bool {
    words.contains(&ident)
}

fn keyword_style() -> Style {
    Style::default().fg(CODE_KEYWORD_FG)
}

fn type_style() -> Style {
    Style::default().fg(CODE_TYPE_FG)
}

fn function_style() -> Style {
    Style::default().fg(CODE_FUNCTION_FG)
}

fn string_style() -> Style {
    Style::default().fg(CODE_STRING_FG)
}

fn comment_style() -> Style {
    Style::default()
        .fg(CODE_COMMENT_FG)
        .add_modifier(Modifier::ITALIC)
}

fn number_style() -> Style {
    Style::default().fg(CODE_NUMBER_FG)
}

fn attr_style() -> Style {
    Style::default().fg(CODE_ATTR_FG)
}

fn wrap_spans_with_prefix(
    spans: Vec<Span<'static>>,
    first_prefix: Vec<Span<'static>>,
    continuation_prefix: Vec<Span<'static>>,
    content_width: usize,
) -> Vec<Line<'static>> {
    let limit = content_width.max(1);
    let mut lines = Vec::new();
    let mut current = first_prefix;
    let mut current_width = spans_width(&current);
    let mut prefix_width = current_width;

    for span in spans {
        for ch in span.content.chars() {
            if ch == '\n' {
                lines.push(Line::from(std::mem::take(&mut current)));
                current = continuation_prefix.clone();
                current_width = spans_width(&current);
                prefix_width = current_width;
                continue;
            }

            let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
            if current_width > prefix_width && current_width + ch_width > limit {
                lines.push(Line::from(std::mem::take(&mut current)));
                current = continuation_prefix.clone();
                current_width = spans_width(&current);
                prefix_width = current_width;
            }

            push_char_span(&mut current, ch, span.style);
            current_width += ch_width;
        }
    }

    lines.push(Line::from(current));
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

fn table_column_widths(
    table: &TableState,
    column_count: usize,
    content_width: usize,
) -> Vec<usize> {
    if content_width == 0 || column_count == 0 {
        return Vec::new();
    }

    let border_and_padding_width = 1 + column_count * 3;
    let available = content_width.saturating_sub(border_and_padding_width);
    if available == 0 {
        return Vec::new();
    }

    let mut widths = vec![1usize; column_count];
    for (idx, cell) in table.header.iter().enumerate().take(column_count) {
        widths[idx] = widths[idx].max(UnicodeWidthStr::width(cell.as_str()));
    }
    for row in &table.rows {
        for (idx, cell) in row.iter().enumerate().take(column_count) {
            widths[idx] = widths[idx].max(UnicodeWidthStr::width(cell.as_str()));
        }
    }

    distribute_table_widths(widths, available)
}

fn distribute_table_widths(desired_widths: Vec<usize>, available: usize) -> Vec<usize> {
    if desired_widths.is_empty() || available == 0 {
        return Vec::new();
    }

    let column_count = desired_widths.len();
    let mut widths = vec![1usize; column_count];
    let mut remaining = available.saturating_sub(column_count);

    while remaining > 0 {
        let growable = desired_widths
            .iter()
            .zip(widths.iter())
            .filter(|(desired, width)| *width < *desired)
            .count();
        if growable == 0 {
            break;
        }

        let step = remaining.div_ceil(growable).max(1);
        let mut grew = false;
        for (idx, desired) in desired_widths.iter().copied().enumerate() {
            if widths[idx] >= desired {
                continue;
            }
            let growth = step.min(desired - widths[idx]).min(remaining);
            widths[idx] += growth;
            remaining -= growth;
            grew = true;
            if remaining == 0 {
                break;
            }
        }

        if !grew {
            break;
        }
    }

    widths
}

fn visible_table_column_count(column_count: usize, content_width: usize) -> Option<usize> {
    if column_count == 0 || content_width < 5 {
        return None;
    }
    Some(column_count.min((content_width - 1) / 4).max(1))
}

fn render_table_rows(
    row: &[String],
    widths: &[usize],
    alignments: &[Alignment],
    header: bool,
) -> Vec<Line<'static>> {
    let cell_style = if header {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    let divider_style = Style::default().fg(TABLE_BORDER);
    let wrapped_cells = widths
        .iter()
        .copied()
        .enumerate()
        .map(|(idx, width)| {
            let cell = row.get(idx).map_or("", String::as_str);
            wrap_table_cell(cell, width)
        })
        .collect::<Vec<_>>();
    let row_height = wrapped_cells.iter().map(Vec::len).max().unwrap_or(1).max(1);
    let mut lines = Vec::with_capacity(row_height);

    for line_idx in 0..row_height {
        let mut spans = Vec::new();
        spans.push(Span::styled("\u{2502}", divider_style));
        for (idx, width) in widths.iter().copied().enumerate() {
            if idx > 0 {
                spans.push(Span::styled("\u{2502}", divider_style));
            }

            let cell_line = wrapped_cells
                .get(idx)
                .and_then(|cell_lines| cell_lines.get(line_idx))
                .map_or("", String::as_str);
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                align_cell(
                    cell_line,
                    width,
                    alignments.get(idx).copied().unwrap_or(Alignment::None),
                ),
                cell_style,
            ));
            spans.push(Span::raw(" "));
        }
        spans.push(Span::styled("\u{2502}", divider_style));
        lines.push(Line::from(spans));
    }

    lines
}

fn render_table_border(widths: &[usize], left: char, junction: char, right: char) -> Line<'static> {
    let mut text = String::new();
    let style = Style::default().fg(TABLE_BORDER);

    text.push(left);
    for (idx, width) in widths.iter().copied().enumerate() {
        if idx > 0 {
            text.push(junction);
        }
        text.push_str(&"\u{2500}".repeat(width + 2));
    }
    text.push(right);

    Line::from(Span::styled(text, style))
}

fn render_table_padding_row(widths: &[usize]) -> Line<'static> {
    let mut spans = Vec::new();
    let divider_style = Style::default().fg(TABLE_BORDER);

    spans.push(Span::styled("\u{2502}", divider_style));
    for (idx, width) in widths.iter().copied().enumerate() {
        if idx > 0 {
            spans.push(Span::styled("\u{2502}", divider_style));
        }
        spans.push(Span::raw(" ".repeat(width + 2)));
    }
    spans.push(Span::styled("\u{2502}", divider_style));

    Line::from(spans)
}

fn wrap_table_cell(cell: &str, width: usize) -> Vec<String> {
    if cell.is_empty() {
        return vec![String::new()];
    }

    let mut lines = Vec::new();
    for logical in cell.split('\n') {
        let mut wrapped = wrap_display_width(logical, width);
        if wrapped.is_empty() {
            lines.push(String::new());
        } else {
            lines.append(&mut wrapped);
        }
    }
    lines
}

fn wrap_display_width(text: &str, max_width: usize) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }

    let limit = max_width.max(1);
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;

    for ch in text.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if current_width > 0 && current_width + ch_width > limit {
            lines.push(std::mem::take(&mut current));
            current_width = 0;
        }
        current.push(ch);
        current_width += ch_width;
    }

    lines.push(current);
    lines
}

fn align_cell(cell: &str, width: usize, alignment: Alignment) -> String {
    let cell_width = UnicodeWidthStr::width(cell);
    if cell_width >= width {
        return cell.to_string();
    }

    let padding = width - cell_width;
    match alignment {
        Alignment::Right => format!("{}{}", " ".repeat(padding), cell),
        Alignment::Center => {
            let left = padding / 2;
            let right = padding - left;
            format!("{}{}{}", " ".repeat(left), cell, " ".repeat(right))
        }
        Alignment::Left | Alignment::None => format!("{}{}", cell, " ".repeat(padding)),
    }
}

fn spans_width(spans: &[Span<'_>]) -> usize {
    spans
        .iter()
        .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
        .sum()
}

fn is_blank_line(line: &Line<'_>) -> bool {
    line.spans
        .iter()
        .all(|span| span.content.as_ref().trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
    }

    fn has_fg(lines: &[Line<'_>], text: &str, color: Color) -> bool {
        lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref().contains(text) && span.style.fg == Some(color))
        })
    }

    fn has_exact_fg(lines: &[Line<'_>], text: &str, color: Color) -> bool {
        lines.iter().any(|line| {
            line.spans
                .iter()
                .any(|span| span.content.as_ref() == text && span.style.fg == Some(color))
        })
    }

    #[test]
    fn renders_heading_as_styled_line() {
        let lines = build_markdown_lines("## Result", 40);

        assert_eq!(plain(&lines[0]), "\u{00b7} Result");
        assert!(
            lines[0]
                .spans
                .iter()
                .any(|span| span.style.add_modifier.contains(Modifier::BOLD))
        );
    }

    #[test]
    fn renders_fenced_code_block_without_panel_background() {
        let lines = build_markdown_lines("```rust\nfn main() {\n    println!(\"hi\");\n}\n```", 32);
        let plain_lines = lines.iter().map(plain).collect::<Vec<_>>();

        assert!(plain_lines.iter().any(|line| line.contains("fn main()")));
        assert!(plain_lines.iter().any(|line| line.contains("    println!")));
        assert!(
            plain_lines.iter().all(|line| !line.starts_with('\u{2502}')),
            "code block should not use a leading rail"
        );
        assert!(
            lines.iter().all(|line| line.style.bg.is_none()),
            "code block should not use a block background"
        );
    }

    #[test]
    fn highlights_plain_text_tokens_without_changing_text() {
        let input =
            "Run cargo test -p omini-tui in crates/omini-tui/src/markdown.rs for @agent #123.";
        let lines = build_markdown_lines(input, 120);

        assert_eq!(plain(&lines[0]), input);
        assert!(has_exact_fg(&lines, "cargo", TEXT_COMMAND_FG));
        assert!(has_exact_fg(&lines, "-p", TEXT_FLAG_FG));
        assert!(has_exact_fg(
            &lines,
            "crates/omini-tui/src/markdown.rs",
            TEXT_PATH_FG
        ));
        assert!(has_exact_fg(&lines, "@agent", TEXT_REF_FG));
        assert!(has_exact_fg(&lines, "#123", TEXT_REF_FG));
        assert!(!has_exact_fg(&lines, "#123.", TEXT_REF_FG));
    }

    #[test]
    fn highlights_rust_code_tokens() {
        let lines = build_markdown_lines(
            "```rust\nfn main() {\n    println!(\"hi\"); // ok\n}\n```",
            80,
        );

        assert!(has_exact_fg(&lines, "fn", CODE_KEYWORD_FG));
        assert!(has_exact_fg(&lines, "main", CODE_FUNCTION_FG));
        assert!(has_exact_fg(&lines, "println!", CODE_FUNCTION_FG));
        assert!(has_exact_fg(&lines, "\"hi\"", CODE_STRING_FG));
        assert!(has_fg(&lines, "// ok", CODE_COMMENT_FG));
    }

    #[test]
    fn highlights_common_language_code_blocks() {
        let samples = [
            (
                "python",
                "def load(x):\n    return str(x) # ok",
                "def",
                "load",
            ),
            (
                "javascript",
                "function run() { return 1; // ok }",
                "function",
                "run",
            ),
            (
                "typescript",
                "const value: string = format(1);",
                "const",
                "format",
            ),
            ("go", "func main() {\nprintln(\"hi\")\n}", "func", "main"),
            (
                "java",
                "class App { void run() { return; } }",
                "class",
                "run",
            ),
            (
                "c",
                "#include <stdio.h>\nint main() { return 0; }",
                "return",
                "main",
            ),
            (
                "cpp",
                "template <typename T>\nvoid run(T value) { return; }",
                "template",
                "run",
            ),
        ];

        for (language, code, keyword, function) in samples {
            let input = format!("```{language}\n{code}\n```");
            let lines = build_markdown_lines(&input, 120);

            assert!(
                has_exact_fg(&lines, keyword, CODE_KEYWORD_FG),
                "{language} keyword {keyword} should be highlighted"
            );
            assert!(
                has_exact_fg(&lines, function, CODE_FUNCTION_FG),
                "{language} function {function} should be highlighted"
            );
        }
    }

    #[test]
    fn leaves_unknown_code_blocks_plain() {
        let lines = build_markdown_lines("```madeup\nalpha beta(1)\n```", 40);

        assert_eq!(plain(&lines[0]), "alpha beta(1)");
        assert!(
            lines
                .iter()
                .flat_map(|line| line.spans.iter())
                .all(|span| span.style.fg.is_none())
        );
    }

    #[test]
    fn renders_tables_with_aligned_columns() {
        let input = "| Name | Count |\n| --- | ---: |\n| apple | 12 |\n| banana | 3 |";
        let lines = build_markdown_lines(input, 32);
        let plain_lines = lines.iter().map(plain).collect::<Vec<_>>();

        assert_eq!(
            plain_lines[0],
            "\u{250c}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{252c}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2510}"
        );
        assert_eq!(plain_lines[1], "\u{2502} Name   \u{2502} Count \u{2502}");
        assert!(
            lines[1]
                .spans
                .iter()
                .any(|span| { span.content.as_ref().contains("Name") && span.style.fg.is_none() })
        );
        assert!(
            lines[1]
                .spans
                .iter()
                .any(|span| { span.content.as_ref().contains("Count") && span.style.fg.is_none() })
        );
        assert_eq!(
            plain_lines[2],
            "\u{251c}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{253c}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2524}"
        );
        assert_eq!(plain_lines[3], "\u{2502} apple  \u{2502}    12 \u{2502}");
        assert_eq!(
            plain_lines[4],
            "\u{251c}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{253c}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2524}"
        );
        assert_eq!(plain_lines[5], "\u{2502} banana \u{2502}     3 \u{2502}");
        assert_eq!(plain_lines[6], "\u{2502}        \u{2502}       \u{2502}");
        assert_eq!(
            plain_lines[7],
            "\u{2514}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2534}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2518}"
        );
    }

    #[test]
    fn wraps_wide_table_cells() {
        let input = "| Path |\n| --- |\n| abcdefghijklmnop |";
        let lines = build_markdown_lines(input, 16);
        let plain_lines = lines.iter().map(plain).collect::<Vec<_>>();

        assert!(
            lines
                .iter()
                .all(|line| UnicodeWidthStr::width(plain(line).as_str()) <= 16)
        );
        assert!(!plain_lines.iter().any(|line| line.contains('\u{2026}')));
        assert!(plain_lines.iter().any(|line| line.contains("abcdefghijkl")));
        assert!(plain_lines.iter().any(|line| line.contains("mnop")));
    }

    #[test]
    fn table_long_columns_use_available_terminal_width() {
        let input = concat!(
            "| A | B | Rule |\n",
            "| --- | --- | --- |\n",
            "| x | y | abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz |",
        );
        let lines = build_markdown_lines(input, 60);
        let plain_lines = lines.iter().map(plain).collect::<Vec<_>>();

        assert_eq!(UnicodeWidthStr::width(plain_lines[0].as_str()), 60);
        assert!(
            plain_lines
                .iter()
                .all(|line| UnicodeWidthStr::width(line.as_str()) <= 60)
        );
        assert!(
            plain_lines
                .iter()
                .any(|line| line.contains("abcdefghijklmnopqrstuvwxyz"))
        );
    }

    #[test]
    fn renders_inline_code_as_foreground_highlight_only() {
        let lines = build_markdown_lines("Use `cargo test` now", 40);
        let code_span = lines[0]
            .spans
            .iter()
            .find(|span| span.content.as_ref() == "cargo test")
            .expect("inline code span");

        assert_eq!(code_span.style.fg, Some(INLINE_CODE_FG));
        assert_eq!(code_span.style.bg, None);
    }

    #[test]
    fn renders_blockquotes_without_leading_rail() {
        let lines = build_markdown_lines("> quoted text", 40);

        assert_eq!(plain(&lines[0]), "quoted text");
        assert_eq!(lines[0].spans[0].style.fg, Some(DIM));
    }

    #[test]
    fn treats_unclosed_code_fence_as_code_block() {
        let lines = build_markdown_lines("```text\npartial", 20);
        let plain_lines = lines.iter().map(plain).collect::<Vec<_>>();

        assert!(plain_lines.iter().any(|line| line.contains("partial")));
    }
}
