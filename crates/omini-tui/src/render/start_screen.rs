use super::register_selectable_lines;
use super::text::truncate_str;
use super::thread_list::relative_time;
use crate::state::UiState;
use crate::types::config::ThinkingEffort;
use crate::types::events::CommandKind;
use omini_domain::display::MentionKind;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const LOGO: &str = include_str!("../../assets/omini-logo.txt");

pub(super) fn render_start_screen(state: &mut UiState, frame: &mut ratatui::Frame, area: Rect) {
    state.selectable_message_lines.clear();
    state.message_scroll_y = 0;
    state.total_lines = 0;

    if area.width == 0 || area.height == 0 {
        return;
    }

    let lines = start_screen_lines(state, area.width as usize, area.height as usize);
    register_selectable_lines(state, area, &lines);
    frame.render_widget(Paragraph::new(lines), area);
}

fn start_screen_lines(state: &UiState, width: usize, height: usize) -> Vec<Line<'static>> {
    if width < 38 || height < 8 {
        return compact_lines(width);
    }
    if width < 96 || height < 12 {
        return medium_lines(state, width);
    }
    full_lines(state, width)
}

fn full_lines(state: &UiState, width: usize) -> Vec<Line<'static>> {
    let panel_width = width;
    let inner_width = panel_width.saturating_sub(2);
    let left_width = ((inner_width * 34) / 100).clamp(44, 56);
    let right_width = inner_width.saturating_sub(left_width + 1);
    if right_width < 36 {
        return medium_lines(state, width);
    }

    let logo_width = LOGO.lines().map(UnicodeWidthStr::width).max().unwrap_or(0);
    let logo_lines = if logo_width <= left_width {
        LOGO.lines()
            .map(|line| vec![Span::styled(line.to_string(), logo_style())])
            .collect::<Vec<_>>()
    } else {
        vec![vec![Span::styled("omini", logo_style())]]
    };

    let mut lines = vec![
        top_border(panel_width, " omini "),
        split_row(
            empty_cell(left_width),
            right_cell(
                vec![Span::styled("Startup Tip", heading_style())],
                right_width,
            ),
        ),
    ];
    lines.push(split_row(
        empty_cell(left_width),
        right_cell(
            vec![Span::styled(state.startup_tip.clone(), muted_value_style())],
            right_width,
        ),
    ));
    lines.push(split_row(
        centered_cell(
            vec![Span::styled("Welcome back!", value_style())],
            left_width,
        ),
        empty_cell(right_width),
    ));
    lines.push(right_separator(left_width, right_width));
    lines.push(split_row(
        empty_cell(left_width),
        right_heading("Recent Sessions", right_width),
    ));
    let right_project_rows = project_overview_rows(state, right_width);
    let total_rows = logo_lines.len().max(right_project_rows.len());
    for idx in 0..total_rows {
        let logo = logo_lines.get(idx);
        let row = right_project_rows
            .get(idx)
            .cloned()
            .unwrap_or_else(Vec::new);
        lines.push(split_row(
            logo.map_or_else(
                || empty_cell(left_width),
                |l| centered_cell(l.clone(), left_width),
            ),
            right_cell(row, right_width),
        ));
    }
    lines.push(split_row(empty_cell(left_width), empty_cell(right_width)));
    lines.push(split_row(
        centered_cell(model_metadata_spans(state), left_width),
        empty_cell(right_width),
    ));
    lines.push(split_row(
        centered_cell(
            vec![Span::styled(compact_path(state), muted_value_style())],
            left_width,
        ),
        empty_cell(right_width),
    ));
    lines.push(bottom_border(panel_width));
    lines
}

fn project_overview_rows(state: &UiState, right_width: usize) -> Vec<Vec<Span<'static>>> {
    // right_cell (CellAlign::Left) adds 2 chars of left padding.
    // Available width for row content:
    let content_available = right_width.saturating_sub(2);

    // 计算时间列最大显示宽度
    let max_time_width = state
        .startup_recent_threads
        .iter()
        .take(6)
        .map(|s| UnicodeWidthStr::width(relative_time(s.updated_at).as_str()))
        .max()
        .unwrap_or(0);

    // 标题可用宽度 = 总可用 - 时间列 - 分隔符("  ")
    let title_max_width = content_available
        .saturating_sub(max_time_width)
        .saturating_sub(2);

    let mut rows = state
        .startup_recent_threads
        .iter()
        .take(6)
        .map(|thread| {
            let time = relative_time(thread.updated_at);
            let time_width = UnicodeWidthStr::width(time.as_str());
            // 左对齐时间列
            let time_padded = format!("{}{}", time, " ".repeat(max_time_width - time_width));
            let title = truncate_str(thread.title.trim(), title_max_width);
            vec![
                Span::styled(time_padded, label_style()),
                Span::styled("  ", muted_value_style()),
                Span::styled(title, value_style()),
            ]
        })
        .collect::<Vec<_>>();

    // 帮助信息行，从最左边开始（与时间列左边缘对齐）
    let help_items: Vec<(&'static str, &'static str)> = vec![
        ("/sessions", " 恢复历史会话"),
        ("@文件 或 @目录", " 限定上下文"),
        ("/help", " 查看命令和输入技巧"),
        ("/plan", " 进入 plan 模式先规划再行动"),
        ("/compact", " 压缩上下文保留关键线索"),
        ("/agents", " 查看和管理可用 subagent"),
    ];

    for (cmd, desc) in &help_items {
        if rows.len() >= 6 {
            break;
        }
        let combined = format!("{}{}", cmd, desc);
        let truncated = truncate_str(&combined, content_available);
        // 将截断后的文本按命令部分和描述部分拆分，保留高亮样式
        let cmd_width = UnicodeWidthStr::width(*cmd);
        if UnicodeWidthStr::width(truncated.as_str()) <= cmd_width {
            // 整个截断文本都在命令部分内
            rows.push(vec![command_span_in_text(&truncated, cmd)]);
        } else {
            // 命令部分完整显示，截断发生在描述部分
            let desc_truncated = truncate_str(desc, content_available - cmd_width);
            rows.push(vec![
                command_span(cmd),
                Span::styled(desc_truncated, muted_value_style()),
            ]);
        }
    }

    rows.push(vec![Span::styled(
        format!(
            "{} 个命令 · {} 个 skill · {} 个 agent · {} 个 MCP",
            builtin_command_count(state),
            skill_command_count(state),
            agent_count(state),
            state.startup_mcp_server_count
        ),
        value_style(),
    )]);
    rows
}

/// 在已截断的文本中查找并高亮命令部分。
fn command_span_in_text(text: &str, command: &'static str) -> Span<'static> {
    if text.starts_with(command) {
        Span::styled(
            command.to_string(),
            accent_style().add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(
            text.to_string(),
            accent_style().add_modifier(Modifier::BOLD),
        )
    }
}

fn medium_lines(state: &UiState, width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    lines.extend(logo_lines(width));
    lines.push(Line::from(""));
    lines.push(centered_spans(
        vec![
            Span::styled(state.status_bar.model.clone(), value_style()),
            Span::styled(" · ", label_style()),
            Span::styled(compact_path(state), muted_value_style()),
        ],
        width,
    ));
    lines.push(command_strip(width));
    lines
}

fn compact_lines(width: usize) -> Vec<Line<'static>> {
    vec![
        centered_spans(vec![Span::styled("omini", logo_style())], width),
        command_strip(width),
    ]
}

fn logo_lines(width: usize) -> Vec<Line<'static>> {
    let logo_width = LOGO.lines().map(UnicodeWidthStr::width).max().unwrap_or(0);
    if width < logo_width {
        return vec![centered_spans(
            vec![Span::styled("omini", logo_style())],
            width,
        )];
    }

    LOGO.lines()
        .map(|line| centered_spans(vec![Span::styled(line.to_string(), logo_style())], width))
        .collect()
}

fn command_strip(width: usize) -> Line<'static> {
    centered_spans(
        vec![
            command_span("/help"),
            Span::styled(" commands  ", label_style()),
            command_span("/sessions"),
            Span::styled(" resume  ", label_style()),
            command_span("/agents"),
            Span::styled(" manage  ", label_style()),
            command_span("@"),
            Span::styled(" mention", label_style()),
        ],
        width,
    )
}

fn command_span(command: &'static str) -> Span<'static> {
    Span::styled(command, accent_style().add_modifier(Modifier::BOLD))
}

fn centered_spans(mut spans: Vec<Span<'static>>, width: usize) -> Line<'static> {
    let line_width = spans_width(&spans);
    let padding = width.saturating_sub(line_width) / 2;
    if padding > 0 {
        spans.insert(0, Span::raw(" ".repeat(padding)));
    }
    Line::from(spans)
}

fn top_border(width: usize, title: &'static str) -> Line<'static> {
    let title_width = UnicodeWidthStr::width(title);
    let remaining = width.saturating_sub(title_width + 2);
    Line::from(vec![
        Span::styled("╭", border_style()),
        Span::styled("─", border_style()),
        Span::styled(title, heading_style()),
        Span::styled("─".repeat(remaining.saturating_sub(1)), border_style()),
        Span::styled("╮", border_style()),
    ])
}

fn bottom_border(width: usize) -> Line<'static> {
    if width < 2 {
        return Line::from("");
    }
    Line::from(Span::styled(
        format!("╰{}╯", "─".repeat(width.saturating_sub(2))),
        border_style(),
    ))
}

fn right_separator(left_width: usize, right_width: usize) -> Line<'static> {
    let left_pad = 2.min(right_width);
    let horizontal_width = right_width.saturating_sub(left_pad + 2);
    let right_pad = right_width.saturating_sub(left_pad + horizontal_width);
    Line::from(vec![
        Span::styled("│", border_style()),
        Span::raw(" ".repeat(left_width)),
        Span::styled("│", border_style()),
        Span::raw(" ".repeat(left_pad)),
        Span::styled("─".repeat(horizontal_width), border_style()),
        Span::raw(" ".repeat(right_pad)),
        Span::styled("│", border_style()),
    ])
}

fn split_row(left: Vec<Span<'static>>, right: Vec<Span<'static>>) -> Line<'static> {
    let mut spans = vec![Span::styled("│", border_style())];
    spans.extend(left);
    spans.push(Span::styled("│", border_style()));
    spans.extend(right);
    spans.push(Span::styled("│", border_style()));
    Line::from(spans)
}

fn empty_cell(width: usize) -> Vec<Span<'static>> {
    vec![Span::raw(" ".repeat(width))]
}

fn right_heading(text: &'static str, width: usize) -> Vec<Span<'static>> {
    right_cell(vec![Span::styled(text, heading_style())], width)
}

fn right_cell(spans: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    padded_cell(spans, width, CellAlign::Left)
}

fn centered_cell(spans: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    padded_cell(spans, width, CellAlign::Center)
}

#[derive(Clone, Copy)]
enum CellAlign {
    Left,
    Center,
}

fn padded_cell(spans: Vec<Span<'static>>, width: usize, align: CellAlign) -> Vec<Span<'static>> {
    let content_width = spans_width(&spans);
    let spans = if content_width > width {
        vec![Span::styled(
            truncate_spans_text(&spans, width),
            muted_value_style(),
        )]
    } else {
        spans
    };
    let content_width = spans_width(&spans);
    let remaining = width.saturating_sub(content_width);
    let left_pad = match align {
        CellAlign::Left => 2.min(remaining),
        CellAlign::Center => remaining / 2,
    };
    let right_pad = remaining.saturating_sub(left_pad);
    let mut out = Vec::new();
    if left_pad > 0 {
        out.push(Span::raw(" ".repeat(left_pad)));
    }
    out.extend(spans);
    if right_pad > 0 {
        out.push(Span::raw(" ".repeat(right_pad)));
    }
    out
}

fn truncate_spans_text(spans: &[Span<'static>], width: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for ch in spans.iter().flat_map(|span| span.content.chars()) {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + ch_width > width {
            break;
        }
        out.push(ch);
        used += ch_width;
    }
    out
}

fn spans_width(spans: &[Span<'static>]) -> usize {
    spans
        .iter()
        .flat_map(|span| span.content.chars())
        .map(|ch| UnicodeWidthChar::width(ch).unwrap_or(0))
        .sum()
}

fn compact_path(state: &UiState) -> String {
    let path = &state.status_bar.cwd;
    let home = std::env::var("HOME").unwrap_or_default();
    let raw = path.to_string_lossy();
    if !home.is_empty() && raw.starts_with(&home) {
        format!("~{}", &raw[home.len()..])
    } else {
        raw.to_string()
    }
}

fn model_label(state: &UiState) -> String {
    if state.status_bar.model.is_empty() {
        "unknown model".to_string()
    } else {
        state.status_bar.model.clone()
    }
}

fn provider_label(state: &UiState) -> String {
    if state.status_bar.active_provider.is_empty() {
        "unknown".to_string()
    } else {
        state.status_bar.active_provider.clone()
    }
}

fn model_metadata_spans(state: &UiState) -> Vec<Span<'static>> {
    let mut spans = vec![Span::styled(model_label(state), value_style())];
    if let Some(effort) = thinking_effort_label(state) {
        spans.push(Span::styled(" with ", label_style()));
        spans.push(Span::styled(effort, accent_style()));
    }
    spans.push(Span::styled(" · ", label_style()));
    spans.push(Span::styled(provider_label(state), value_style()));
    spans
}

fn thinking_effort_label(state: &UiState) -> Option<&'static str> {
    match state.status_bar.thinking_effort {
        Some(ThinkingEffort::Low) => Some("low"),
        Some(ThinkingEffort::Medium) => Some("medium"),
        Some(ThinkingEffort::High) => Some("high"),
        Some(ThinkingEffort::XHigh) => Some("xhigh"),
        Some(ThinkingEffort::Max) => Some("max"),
        Some(ThinkingEffort::None) | None => None,
    }
}

fn builtin_command_count(state: &UiState) -> usize {
    state
        .autocomplete
        .all_commands
        .iter()
        .filter(|command| command.kind == CommandKind::Builtin)
        .count()
}

fn skill_command_count(state: &UiState) -> usize {
    state
        .autocomplete
        .all_commands
        .iter()
        .filter(|command| command.kind == CommandKind::Skill)
        .count()
}

fn agent_count(state: &UiState) -> usize {
    state
        .mention_autocomplete
        .all_candidates
        .iter()
        .filter(|candidate| candidate.kind == MentionKind::Subagent)
        .count()
}

fn logo_style() -> Style {
    Style::default()
        .fg(Color::Rgb(0x42, 0xd9, 0xe8))
        .add_modifier(Modifier::BOLD)
}

fn border_style() -> Style {
    Style::default().fg(Color::Rgb(0x35, 0x8c, 0x98))
}

fn heading_style() -> Style {
    Style::default()
        .fg(Color::Rgb(0x42, 0xd9, 0xe8))
        .add_modifier(Modifier::BOLD)
}

fn label_style() -> Style {
    Style::default().fg(Color::Rgb(0x7a, 0x82, 0x8e))
}

fn value_style() -> Style {
    Style::default().fg(Color::Rgb(0xc6, 0xd0, 0xdc))
}

fn muted_value_style() -> Style {
    Style::default().fg(Color::Rgb(0xa0, 0xa7, 0xb2))
}

fn accent_style() -> Style {
    Style::default().fg(Color::Rgb(0x42, 0xd9, 0xe8))
}
