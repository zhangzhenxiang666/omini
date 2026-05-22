use crate::state::{AgentStatus, InteractionStep, UiState, format_run_duration};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

const PLAN_APPROVAL_TOP_SPACER_HEIGHT: u16 = 1;
const PLAN_APPROVAL_MIN_MESSAGES_HEIGHT: u16 = 1;
const PLAN_APPROVAL_MESSAGE_GAP_HEIGHT: u16 = 1;

pub(super) fn render(state: &mut UiState, frame: &mut ratatui::Frame) {
    let area = frame.area();
    state.clear_selectable_screen_lines();
    render_background(frame, area);

    if let Some(InteractionStep::Session { .. }) = &state.interaction_step {
        super::render_session_list(state, frame, area);
        return;
    }

    if state.plan_approval.is_some() {
        let reserved_height = PLAN_APPROVAL_TOP_SPACER_HEIGHT
            + PLAN_APPROVAL_MIN_MESSAGES_HEIGHT
            + PLAN_APPROVAL_MESSAGE_GAP_HEIGHT;
        let drawer_height = super::plan_approval_drawer::plan_approval_drawer_height(area)
            .min(area.height.saturating_sub(reserved_height).max(1));
        let chunks = Layout::vertical([
            Constraint::Length(PLAN_APPROVAL_TOP_SPACER_HEIGHT),
            Constraint::Min(PLAN_APPROVAL_MIN_MESSAGES_HEIGHT),
            Constraint::Length(PLAN_APPROVAL_MESSAGE_GAP_HEIGHT),
            Constraint::Length(drawer_height),
        ])
        .split(area);
        state.messages_area = chunks[1];
        super::render_messages(state, frame, chunks[1]);
        super::plan_approval_drawer::render_plan_approval_drawer(state, frame, chunks[3]);
        return;
    }

    let drawer_len = super::input::queued_drawer_inputs(state).len();
    let queued_height = if drawer_len == 0 {
        0
    } else {
        drawer_len.min(4) as u16 + 2
    };
    state.set_input_wrap_width(area.width as usize);
    let input_height = 2 + state.input_visible_line_count() as u16 + queued_height;
    let activity_height = if state.is_run_active() { 3 } else { 1 };
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(activity_height),
        Constraint::Length(input_height),
        Constraint::Length(1),
    ])
    .split(area);
    state.messages_area = chunks[1];

    super::render_messages(state, frame, chunks[1]);
    render_activity(state, frame, chunks[2]);
    super::autocomplete::render_autocomplete(state, frame, chunks[3]);
    super::status::render_footer(state, frame, chunks[4]);

    if state.interaction_step.is_none()
        && state.active_tool_pause().is_none()
        && state.help_drawer.is_none()
        && state.plan_approval.is_none()
    {
        super::input::render_input(state, frame, chunks[3]);
    }

    if state.interaction_request.is_some() {
        super::interactions::render_interaction(state, frame, area);
    }

    super::help_drawer::render_help_drawer(state, frame, area);
    super::permission_drawer::render_permission_drawer(state, frame, area);
}

fn render_background(frame: &mut ratatui::Frame, area: Rect) {
    frame.render_widget(
        Paragraph::new(Line::from("")).style(Style::default().bg(Color::Rgb(40, 44, 52))),
        area,
    );
}

fn render_activity(state: &mut UiState, frame: &mut ratatui::Frame, area: Rect) {
    if area.height == 0 || !state.is_run_active() {
        return;
    }

    let Some(elapsed) = state.current_run_elapsed() else {
        return;
    };

    let activity_area = Rect {
        y: area.y + area.height.saturating_sub(1) / 2,
        height: 1,
        ..area
    };
    let style = Style::default().fg(Color::Rgb(0x7a, 0x82, 0x8e));
    let bright = Color::Rgb(0xa6, 0xaf, 0xb9);
    let dim = Color::Rgb(0x5a, 0x62, 0x6f);
    let label = match state.agent_status {
        AgentStatus::Thinking => "Thinking",
        AgentStatus::Working => "Working",
        AgentStatus::AwaitingInput => "Waiting for you",
        AgentStatus::Idle => return,
    };
    let elapsed = format_run_duration(elapsed);
    let meta = if state.agent_status == AgentStatus::AwaitingInput || state.is_run_timer_paused() {
        format!(" (paused at {elapsed})")
    } else {
        format!(" ({elapsed} · esc to interrupt)")
    };

    let mut spans = vec![Span::styled("• ", style)];
    if state.agent_status == AgentStatus::AwaitingInput {
        spans.push(Span::styled(label.to_string(), style));
    } else {
        spans.extend(super::status::animated_status_spans_with_palette(
            label, bright, dim,
        ));
    }
    spans.push(Span::styled(meta, style));

    let mut line = Line::from(spans);
    super::register_and_highlight_lines(state, activity_area, std::slice::from_mut(&mut line));
    frame.render_widget(Paragraph::new(line), activity_area);
}
