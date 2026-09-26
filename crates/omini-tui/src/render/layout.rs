use crate::state::{AgentStatus, InteractionStep, UiState, format_run_duration};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

const PLAN_APPROVAL_TOP_SPACER_HEIGHT: u16 = 1;
const PLAN_APPROVAL_MIN_MESSAGES_HEIGHT: u16 = 1;
const PLAN_APPROVAL_MESSAGE_GAP_HEIGHT: u16 = 1;
const TOOL_PAUSE_TOP_SPACER_HEIGHT: u16 = 1;
const TOOL_PAUSE_MIN_MESSAGES_HEIGHT: u16 = 1;
const TOOL_PAUSE_MESSAGE_GAP_HEIGHT: u16 = 1;
const TOOL_PAUSE_FOOTER_HEIGHT: u16 = 1;
const BOTTOM_DRAWER_TOP_SPACER_HEIGHT: u16 = 1;
const BOTTOM_DRAWER_MIN_MESSAGES_HEIGHT: u16 = 1;
const BOTTOM_DRAWER_MESSAGE_GAP_HEIGHT: u16 = 1;
const MESSAGE_STATUS_GAP_HEIGHT: u16 = 1;
const MESSAGE_INPUT_GAP_HEIGHT: u16 = 1;

pub(super) fn render(state: &mut UiState, frame: &mut ratatui::Frame) {
    let area = frame.area();
    state.clear_selectable_screen_lines();
    render_background(frame, area);

    if let Some(InteractionStep::Thread { .. }) = &state.interaction_step {
        super::render_thread_list(state, frame, area);
        crate::selection::apply_selection_overlay(state, frame.buffer_mut());
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
        crate::selection::apply_selection_overlay(state, frame.buffer_mut());
        return;
    }

    if state.active_tool_pause().is_some() {
        let reserved_height = TOOL_PAUSE_TOP_SPACER_HEIGHT
            + TOOL_PAUSE_MIN_MESSAGES_HEIGHT
            + TOOL_PAUSE_MESSAGE_GAP_HEIGHT
            + TOOL_PAUSE_FOOTER_HEIGHT;
        let drawer_height = super::permission_drawer::permission_drawer_height(state, area)
            .min(area.height.saturating_sub(reserved_height).max(1));
        let chunks = Layout::vertical([
            Constraint::Length(TOOL_PAUSE_TOP_SPACER_HEIGHT),
            Constraint::Min(TOOL_PAUSE_MIN_MESSAGES_HEIGHT),
            Constraint::Length(TOOL_PAUSE_MESSAGE_GAP_HEIGHT),
            Constraint::Length(drawer_height),
            Constraint::Length(TOOL_PAUSE_FOOTER_HEIGHT),
        ])
        .split(area);
        state.messages_area = chunks[1];
        super::render_messages(state, frame, chunks[1]);
        super::status::render_footer(state, frame, chunks[4]);

        if state.interaction_request.is_some() {
            super::interactions::render_interaction(state, frame, area);
        }

        super::help_drawer::render_help_drawer(state, frame, area);
        super::permission_drawer::render_permission_drawer(state, frame, chunks[3]);
        crate::selection::apply_selection_overlay(state, frame.buffer_mut());
        return;
    }

    if let Some(drawer_height) = bottom_drawer_height(state, area) {
        let reserved_height = BOTTOM_DRAWER_TOP_SPACER_HEIGHT
            + BOTTOM_DRAWER_MIN_MESSAGES_HEIGHT
            + BOTTOM_DRAWER_MESSAGE_GAP_HEIGHT;
        let drawer_height = drawer_height.min(area.height.saturating_sub(reserved_height).max(1));
        let chunks = Layout::vertical([
            Constraint::Length(BOTTOM_DRAWER_TOP_SPACER_HEIGHT),
            Constraint::Min(BOTTOM_DRAWER_MIN_MESSAGES_HEIGHT),
            Constraint::Length(BOTTOM_DRAWER_MESSAGE_GAP_HEIGHT),
            Constraint::Length(drawer_height),
        ])
        .split(area);
        state.messages_area = chunks[1];
        super::render_messages(state, frame, chunks[1]);

        if state.help_drawer.is_some() {
            super::help_drawer::render_help_drawer(state, frame, chunks[3]);
        } else if super::interactions::interaction_drawer_height(state, area).is_some() {
            super::interactions::render_interaction(state, frame, chunks[3]);
        }
        crate::selection::apply_selection_overlay(state, frame.buffer_mut());
        return;
    }

    let drawer_len = super::input::queued_drawer_inputs(state).len();
    let queued_height = if drawer_len == 0 {
        0
    } else {
        drawer_len.min(4) as u16 + 2
    };
    let show_start_screen = should_render_start_screen(state);
    state.set_input_wrap_width(area.width as usize);
    let input_height = 2 + state.input_visible_line_count() as u16 + queued_height;
    let session_list_height = if state.subagent_order.is_empty() {
        0
    } else {
        state.session_count().min(u16::MAX as usize) as u16
    };
    let session_list_gap_height = u16::from(session_list_height > 0);
    let activity_height = if state.is_run_active() { 1 } else { 0 };
    let activity_gap_height = if activity_height > 0 {
        MESSAGE_STATUS_GAP_HEIGHT
    } else {
        0
    };
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(activity_gap_height),
        Constraint::Length(activity_height),
        Constraint::Length(MESSAGE_INPUT_GAP_HEIGHT),
        Constraint::Length(input_height),
        Constraint::Length(1),
        Constraint::Length(session_list_gap_height),
        Constraint::Length(session_list_height),
    ])
    .split(area);
    state.messages_area = chunks[1];

    if show_start_screen {
        super::render_start_screen(state, frame, chunks[1]);
    } else {
        super::render_messages(state, frame, chunks[1]);
    }
    render_activity(state, frame, chunks[3]);
    super::autocomplete::render_autocomplete(state, frame, chunks[5]);
    super::status::render_footer(state, frame, chunks[6]);
    render_session_selector(state, frame, chunks[8]);

    if state.interaction_step.is_none()
        && state.active_tool_pause().is_none()
        && state.help_drawer.is_none()
        && state.plan_approval.is_none()
    {
        super::input::render_input(state, frame, chunks[5]);
    }

    if state.interaction_request.is_some() {
        super::interactions::render_interaction(state, frame, area);
    }

    super::help_drawer::render_help_drawer(state, frame, area);
    super::permission_drawer::render_permission_drawer(state, frame, area);
    crate::selection::apply_selection_overlay(state, frame.buffer_mut());
}

fn render_session_selector(state: &UiState, frame: &mut ratatui::Frame, area: Rect) {
    if area.height == 0 || state.subagent_order.is_empty() {
        return;
    }
    let dim = Style::default().fg(Color::Rgb(135, 140, 150));
    let active_style = Style::default()
        .fg(Color::Rgb(230, 232, 238))
        .add_modifier(Modifier::BOLD);
    let selected_style = Style::default()
        .fg(Color::Rgb(66, 217, 232))
        .add_modifier(Modifier::BOLD);
    let mut rows = Vec::with_capacity(state.session_count());
    rows.push(("main".to_string(), None, 0usize));
    for (index, task_id) in state.subagent_order.iter().enumerate() {
        let Some(node) = state
            .subagents
            .values()
            .find(|node| &node.task_id == task_id)
        else {
            continue;
        };
        rows.push((
            format!("{}  {}", node.agent_label, node.title),
            Some(node.status),
            index + 1,
        ));
    }
    let lines = rows
        .into_iter()
        .take(area.height as usize)
        .map(|(label, status, index)| {
            let selected = if state.session_selector_focused {
                state.session_selection_index == index
            } else {
                state
                    .active_session_task_id
                    .as_ref()
                    .is_some_and(|task_id| {
                        state.subagent_order.get(index.wrapping_sub(1)) == Some(task_id)
                    })
                    || (index == 0 && state.active_session_task_id.is_none())
            };
            let marker = if selected { "● " } else { "○ " };
            let mut spans = vec![Span::styled(
                marker,
                if selected { selected_style } else { dim },
            )];
            spans.push(Span::styled(
                label,
                if selected { active_style } else { dim },
            ));
            if let Some(status) = status {
                spans.push(Span::styled(
                    format!("  {}", task_status_label(status)),
                    dim,
                ));
            }
            Line::from(spans)
        })
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(lines), area);
}

fn task_status_label(status: omini_domain::task::TaskStatus) -> &'static str {
    use omini_domain::task::TaskStatus;
    match status {
        TaskStatus::Running => "running",
        TaskStatus::Cancelling => "cancelling",
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "failed",
        TaskStatus::Cancelled => "cancelled",
        TaskStatus::Interrupted => "interrupted",
    }
}

fn should_render_start_screen(state: &UiState) -> bool {
    state.show_start_screen
        && state.messages.is_empty()
        && state.pending_assistant.is_none()
        && state.pending_proposed_plan.is_none()
        && state.pending_compact_summary.is_none()
        && state.help_drawer.is_none()
        && state.active_tool_pause().is_none()
        && state.plan_approval.is_none()
}

fn bottom_drawer_height(state: &UiState, area: Rect) -> Option<u16> {
    if state.help_drawer.is_some() {
        return Some(super::help_drawer::help_drawer_height(area));
    }
    super::interactions::interaction_drawer_height(state, area)
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

    let line = Line::from(spans);
    super::register_selectable_lines(state, activity_area, std::slice::from_ref(&line));
    frame.render_widget(Paragraph::new(line), activity_area);
}
