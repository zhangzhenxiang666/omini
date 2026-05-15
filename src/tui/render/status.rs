use crate::tui::state::{AgentStatus, UiState};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use std::time::{SystemTime, UNIX_EPOCH};

pub(super) fn animated_status_spans(text: &str) -> Vec<Span<'static>> {
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

    const BR: u8 = 200;
    const BG: u8 = 169;
    const BB: u8 = 238;
    const DR: u8 = 55;
    const DG: u8 = 47;
    const DB: u8 = 65;

    chars
        .iter()
        .enumerate()
        .map(|(i, &c)| {
            let diff = ((i as f64 - wave_pos + n as f64) % n as f64) - n as f64 / 2.0;
            let normalized = diff / (n as f64 / 2.0);
            let bell = (normalized * std::f64::consts::PI).cos().max(0.0);
            let dim_min = 0.08;
            let brightness = dim_min + (1.0 - dim_min) * bell;

            let r = (DR as f64 + (BR as f64 - DR as f64) * brightness) as u8;
            let g = (DG as f64 + (BG as f64 - DG as f64) * brightness) as u8;
            let b = (DB as f64 + (BB as f64 - DB as f64) * brightness) as u8;

            Span::styled(c.to_string(), Style::default().fg(Color::Rgb(r, g, b)))
        })
        .collect()
}

pub(super) fn render_footer(state: &UiState, frame: &mut ratatui::Frame, area: Rect) {
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
