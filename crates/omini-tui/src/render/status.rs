use crate::selection::{highlighted_line, selected_cols_for_screen_line};
use crate::state::{AgentStatus, UiState};
use crate::types::events::ActiveProfile;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use std::time::{SystemTime, UNIX_EPOCH};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub(super) fn animated_status_spans(text: &str) -> Vec<Span<'static>> {
    animated_status_spans_with_palette(text, Color::Rgb(200, 169, 238), Color::Rgb(55, 47, 65))
}

pub(super) fn animated_status_spans_with_palette(
    text: &str,
    bright: Color,
    dim: Color,
) -> Vec<Span<'static>> {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    if n == 0 {
        return vec![];
    }

    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as f64;
    const CYCLE_MS: f64 = 1200.0;
    let phase = (ms % CYCLE_MS) / CYCLE_MS;
    let wave_pos = phase * n as f64;

    let (br, bg, bb) = color_to_rgb(bright);
    let (dr, dg, db) = color_to_rgb(dim);

    chars
        .iter()
        .enumerate()
        .map(|(i, &c)| {
            let diff = ((i as f64 - wave_pos + n as f64) % n as f64) - n as f64 / 2.0;
            let normalized = diff / (n as f64 / 2.0);
            let bell = (normalized * std::f64::consts::PI).cos().max(0.0);
            let dim_min = 0.08;
            let brightness = dim_min + (1.0 - dim_min) * bell;

            let r = (dr as f64 + (br as f64 - dr as f64) * brightness) as u8;
            let g = (dg as f64 + (bg as f64 - dg as f64) * brightness) as u8;
            let b = (db as f64 + (bb as f64 - db as f64) * brightness) as u8;

            Span::styled(c.to_string(), Style::default().fg(Color::Rgb(r, g, b)))
        })
        .collect()
}

fn color_to_rgb(color: Color) -> (u8, u8, u8) {
    match color {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => (255, 255, 255),
    }
}

pub(super) fn render_footer(state: &mut UiState, frame: &mut ratatui::Frame, area: Rect) {
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
        Some(ThinkingEffort::XHigh) => format!("{}  xhigh", model_part),
        Some(ThinkingEffort::Max) => format!("{}  max", model_part),
        _ => model_part,
    };

    #[cfg(debug_assertions)]
    let debug_session_id = state.current_session_id.as_deref();
    #[cfg(not(debug_assertions))]
    let debug_session_id = None;

    let width = area.width as usize;
    let profile_hint = active_profile_hint(state, width);
    let left_width = left_status_budget(width, profile_hint.as_ref());
    let debug_style = choose_debug_session_style(
        state,
        &model_thinking,
        &path_display,
        debug_session_id,
        left_width,
    );
    let left = build_left_status_line(state, &model_thinking, &path_display, debug_style);
    let mut line = compose_footer_line(left, profile_hint, width);
    state.register_selectable_screen_line(area.y, area.x, area.width, line_to_plain_text(&line));
    apply_selection_highlight(state, area, &mut line);

    let paragraph = Paragraph::new(line).style(Style::default().fg(Color::DarkGray));
    frame.render_widget(paragraph, area);
}

fn append_usage_spans(status_bar: &crate::state::StatusBar, spans: &mut Vec<Span<'static>>) {
    let usage_style = Style::default().fg(Color::Rgb(0xf2, 0xb5, 0x8d));
    if let Some(context_window) = status_bar.context_window
        && context_window > 0
    {
        let percent = ((status_bar.current_context_tokens.max(0) as f64 / context_window as f64)
            * 100.0)
            .round() as i64;
        spans.extend([
            Span::styled(format!(" Context {}% used ", percent.max(0)), usage_style),
            Span::styled("·", Style::default().fg(Color::DarkGray)),
        ]);
    }

    if status_bar.total_tokens > 0 {
        spans.extend([
            Span::styled(
                format!(" {} used ", format_token_count(status_bar.total_tokens)),
                usage_style,
            ),
            Span::styled("·", Style::default().fg(Color::DarkGray)),
        ]);
    }
}

fn format_token_count(tokens: i64) -> String {
    let tokens = tokens.max(0);
    if tokens >= 1_000_000 {
        let millions = tokens as f64 / 1_000_000.0;
        return trim_decimal_unit(millions, "m");
    }
    if tokens >= 1_000 {
        let thousands = tokens as f64 / 1_000.0;
        return trim_decimal_unit(thousands, "k");
    }
    tokens.to_string()
}

fn trim_decimal_unit(value: f64, unit: &str) -> String {
    let mut text = format!("{value:.1}");
    if text.ends_with(".0") {
        text.truncate(text.len() - 2);
    }
    format!("{text}{unit}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DebugSessionStyle {
    Full,
    Short,
    Hidden,
}

fn build_left_status_line(
    state: &UiState,
    model_thinking: &str,
    path_display: &str,
    debug_style: DebugSessionStyle,
) -> Line<'static> {
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
    append_usage_spans(&state.status_bar, &mut base_spans);

    #[cfg(debug_assertions)]
    append_debug_session_spans(
        state.current_session_id.as_deref(),
        debug_style,
        &mut base_spans,
    );

    match &state.agent_status {
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
    }
}

#[cfg(debug_assertions)]
fn append_debug_session_spans(
    session_id: Option<&str>,
    debug_style: DebugSessionStyle,
    spans: &mut Vec<Span<'static>>,
) {
    let Some(session_id) = session_id else {
        return;
    };
    let label = match debug_style {
        DebugSessionStyle::Full => session_id.to_string(),
        DebugSessionStyle::Short => {
            let short = session_id.chars().take(8).collect::<String>();
            format!("sid {short}")
        }
        DebugSessionStyle::Hidden => return,
    };
    spans.extend([
        Span::styled(
            format!(" {label} "),
            Style::default().fg(Color::Rgb(0x8a, 0x8f, 0x98)),
        ),
        Span::styled("·", Style::default().fg(Color::DarkGray)),
    ]);
}

fn choose_debug_session_style(
    state: &UiState,
    model_thinking: &str,
    path_display: &str,
    session_id: Option<&str>,
    width: usize,
) -> DebugSessionStyle {
    if session_id.is_none() {
        return DebugSessionStyle::Hidden;
    }

    for style in [DebugSessionStyle::Full, DebugSessionStyle::Short] {
        let line = build_left_status_line(state, model_thinking, path_display, style);
        if line_width(&line) <= width {
            return style;
        }
    }

    DebugSessionStyle::Hidden
}

fn active_profile_hint(state: &UiState, width: usize) -> Option<Line<'static>> {
    match state.status_bar.active_profile {
        ActiveProfile::Main => None,
        ActiveProfile::Auto => mode_hint(width, "Auto mode", "AUTO", None, auto_mode_hint_style()),
        ActiveProfile::Plan => {
            let suffix =
                (!state.status_bar.plan_mode_message_sent).then_some(" (Shift+Tab 切换模式)");
            mode_hint(width, "Plan mode", "PLAN", suffix, plan_mode_hint_style())
        }
    }
}

fn mode_hint(
    width: usize,
    label: &str,
    compact_label: &str,
    suffix: Option<&str>,
    style: Style,
) -> Option<Line<'static>> {
    if let Some(suffix) = suffix {
        let full = Line::from(vec![
            Span::styled(label.to_string(), style),
            Span::styled(suffix.to_string(), style),
        ]);
        if line_width(&full) < width {
            return Some(full);
        }
    }

    let medium = Line::from(Span::styled(label.to_string(), style));
    if line_width(&medium) < width {
        return Some(medium);
    }

    let compact = Line::from(Span::styled(compact_label.to_string(), style));
    if line_width(&compact) <= width {
        return Some(compact);
    }

    None
}

fn auto_mode_hint_style() -> Style {
    profile_hint_style(Color::Rgb(0x42, 0xd9, 0xe8))
}

fn plan_mode_hint_style() -> Style {
    profile_hint_style(Color::Rgb(0xd7, 0x66, 0xff))
}

fn profile_hint_style(color: Color) -> Style {
    Style::default().fg(color).add_modifier(Modifier::BOLD)
}

fn left_status_budget(width: usize, hint: Option<&Line<'_>>) -> usize {
    let Some(hint) = hint else {
        return width;
    };
    let hint_width = line_width(hint);
    if hint_width >= width {
        0
    } else {
        width.saturating_sub(hint_width + 1)
    }
}

fn compose_footer_line(
    left: Line<'static>,
    hint: Option<Line<'static>>,
    width: usize,
) -> Line<'static> {
    let Some(hint) = hint else {
        return truncate_line_to_width(left, width);
    };
    let hint_width = line_width(&hint);
    if hint_width >= width {
        return truncate_line_to_width(hint, width);
    }

    let left_budget = width.saturating_sub(hint_width + 1);
    let mut line = truncate_line_to_width(left, left_budget);
    let left_width = line_width(&line);
    let gap = width.saturating_sub(left_width + hint_width);
    line.spans.push(Span::raw(" ".repeat(gap)));
    line.spans.extend(hint.spans);
    line
}

fn truncate_line_to_width(line: Line<'static>, width: usize) -> Line<'static> {
    if line_width(&line) <= width {
        return line;
    }

    let mut remaining = width;
    let mut spans = Vec::new();
    for span in line.spans {
        if remaining == 0 {
            break;
        }

        let content = span.content.as_ref();
        let span_width = UnicodeWidthStr::width(content);
        if span_width <= remaining {
            remaining -= span_width;
            spans.push(span);
            continue;
        }

        let truncated = truncate_text_to_width(content, remaining);
        if !truncated.is_empty() {
            spans.push(Span::styled(truncated, span.style));
        }
        break;
    }

    Line::from(spans)
}

fn truncate_text_to_width(text: &str, width: usize) -> String {
    if UnicodeWidthStr::width(text) <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    if width <= 3 {
        return ".".repeat(width);
    }

    let suffix = "...";
    let content_width = width - UnicodeWidthStr::width(suffix);
    let mut used = 0;
    let mut out = String::new();
    for ch in text.chars() {
        let ch_width = ch.width().unwrap_or(0);
        if used + ch_width > content_width {
            break;
        }
        used += ch_width;
        out.push(ch);
    }
    out.push_str(suffix);
    out
}

fn line_width(line: &Line<'_>) -> usize {
    UnicodeWidthStr::width(line_to_plain_text(line).as_str())
}

fn apply_selection_highlight(state: &UiState, area: Rect, line: &mut Line<'static>) {
    let text = line_to_plain_text(line);
    let Some((start_col, end_col)) = selected_cols_for_screen_line(state, area.y, &text) else {
        return;
    };

    let highlight = Style::default()
        .fg(Color::Rgb(40, 44, 52))
        .bg(Color::Rgb(180, 210, 255))
        .add_modifier(ratatui::style::Modifier::BOLD);
    *line = highlighted_line(&text, start_col, end_col, highlight);
}

fn line_to_plain_text(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_mode_hint_is_right_aligned_in_footer() {
        let mut state = UiState::new();
        state.status_bar.active_profile = ActiveProfile::Plan;

        let line = compose_footer_line(Line::from("left"), active_profile_hint(&state, 40), 40);

        let text = line_to_plain_text(&line);
        assert!(text.ends_with("Plan mode (Shift+Tab 切换模式)"));
        assert_eq!(UnicodeWidthStr::width(text.as_str()), 40);
    }

    #[test]
    fn plan_mode_hint_omits_shortcut_after_message_sent() {
        let mut state = UiState::new();
        state.status_bar.active_profile = ActiveProfile::Plan;
        state.status_bar.plan_mode_message_sent = true;

        let line = compose_footer_line(Line::from("left"), active_profile_hint(&state, 40), 40);

        let text = line_to_plain_text(&line);
        assert!(text.ends_with("Plan mode"));
        assert!(!text.contains("Shift+Tab"));
        assert_eq!(UnicodeWidthStr::width(text.as_str()), 40);
    }

    #[test]
    fn auto_mode_hint_is_right_aligned_in_footer() {
        let mut state = UiState::new();
        state.status_bar.active_profile = ActiveProfile::Auto;

        let line = compose_footer_line(Line::from("left"), active_profile_hint(&state, 24), 24);

        let text = line_to_plain_text(&line);
        assert!(text.ends_with("Auto mode"));
        assert_eq!(UnicodeWidthStr::width(text.as_str()), 24);
    }

    #[test]
    fn auto_mode_hint_falls_back_to_compact_label_when_narrow() {
        let mut state = UiState::new();
        state.status_bar.active_profile = ActiveProfile::Auto;

        let line = compose_footer_line(
            Line::from("very long left status"),
            active_profile_hint(&state, 8),
            8,
        );

        let text = line_to_plain_text(&line);
        assert!(text.ends_with("AUTO"));
        assert_eq!(UnicodeWidthStr::width(text.as_str()), 8);
    }

    #[test]
    fn main_profile_omits_plan_mode_hint() {
        let state = UiState::new();

        let line = compose_footer_line(Line::from("left"), active_profile_hint(&state, 40), 40);

        assert_eq!(line_to_plain_text(&line), "left");
    }

    #[test]
    fn plan_mode_hint_falls_back_to_compact_label_when_narrow() {
        let mut state = UiState::new();
        state.status_bar.active_profile = ActiveProfile::Plan;

        let line = compose_footer_line(
            Line::from("very long left status"),
            active_profile_hint(&state, 8),
            8,
        );

        let text = line_to_plain_text(&line);
        assert!(text.ends_with("PLAN"));
        assert_eq!(UnicodeWidthStr::width(text.as_str()), 8);
    }

    #[test]
    fn plan_mode_hint_survives_long_left_status() {
        let mut state = UiState::new();
        state.status_bar.active_profile = ActiveProfile::Plan;

        let line = compose_footer_line(
            Line::from("very long left status that would otherwise hide the mode"),
            active_profile_hint(&state, 24),
            24,
        );

        let text = line_to_plain_text(&line);
        assert!(text.ends_with("Plan mode"));
        assert_eq!(UnicodeWidthStr::width(text.as_str()), 24);
    }

    #[cfg(debug_assertions)]
    #[test]
    fn debug_session_id_does_not_displace_plan_mode_hint() {
        let mut state = UiState::new();
        state.status_bar.active_profile = ActiveProfile::Plan;
        state.status_bar.model = "test-model".to_string();
        state.status_bar.cwd = "/tmp/project".into();
        state.current_session_id = Some("12345678-1234-1234-1234-123456789abc".to_string());
        let width = 24;
        let plan_hint = active_profile_hint(&state, width);
        let left_width = left_status_budget(width, plan_hint.as_ref());
        let debug_style = choose_debug_session_style(
            &state,
            "test-model",
            "/tmp/project",
            state.current_session_id.as_deref(),
            left_width,
        );
        let line = compose_footer_line(
            build_left_status_line(&state, "test-model", "/tmp/project", debug_style),
            plan_hint,
            width,
        );

        let text = line_to_plain_text(&line);
        assert!(text.ends_with("Plan mode"));
        assert_eq!(UnicodeWidthStr::width(text.as_str()), width);
    }

    #[test]
    fn token_count_formats_thousand_and_million_units() {
        assert_eq!(format_token_count(999), "999");
        assert_eq!(format_token_count(538_000), "538k");
        assert_eq!(format_token_count(1_000_000), "1m");
        assert_eq!(format_token_count(1_500_000), "1.5m");
    }

    #[test]
    fn usage_spans_hide_zero_history_usage() {
        let mut status = crate::state::StatusBar {
            current_context_tokens: 56,
            context_window: Some(100),
            ..crate::state::StatusBar::default()
        };
        let mut spans = Vec::new();

        append_usage_spans(&status, &mut spans);
        let text = spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(text.contains("Context 56% used"));
        assert!(!text.contains(" used  ·"));

        status.total_tokens = 1_000_000;
        let mut spans = Vec::new();
        append_usage_spans(&status, &mut spans);
        let text = spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(text.contains("1m used"));
    }
}
