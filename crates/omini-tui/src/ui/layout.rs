use crate::app::state::{AgentStatus, InteractionStep, format_run_duration};
use crate::ui::context::ViewContext;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
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

pub fn render(state: &mut ViewContext<'_>, frame: &mut ratatui::Frame) {
    let area = frame.area();
    state.clear_selectable_screen_lines();
    render_background(frame, area);

    let focus = crate::app::focus::current(state);
    if let Some(InteractionStep::Thread { .. }) = &state.dialogs.interaction_step {
        crate::ui::prelude::render_thread_list(state, frame, area);
        crate::ui::selection::apply_selection_overlay(state, frame.buffer_mut());
        return;
    }

    if let Some(InteractionStep::Agents(manager)) = &state.dialogs.interaction_step {
        let panel_height = crate::features::agents::view::agents_panel_height(manager, area);
        let messages_area = Rect {
            x: area.x,
            y: area.y.saturating_add(1),
            width: area.width,
            height: area.height.saturating_sub(panel_height.saturating_add(2)),
        };
        state.geometry.messages_area = messages_area;
        render_conversation(state, frame, messages_area);
        crate::features::models::view::render_interaction(state, frame, area);
        crate::ui::selection::apply_selection_overlay(state, frame.buffer_mut());
        return;
    }
    if focus == crate::app::focus::Focus::Plan {
        let reserved_height = PLAN_APPROVAL_TOP_SPACER_HEIGHT
            + PLAN_APPROVAL_MIN_MESSAGES_HEIGHT
            + PLAN_APPROVAL_MESSAGE_GAP_HEIGHT;
        let drawer_height =
            crate::ui::prelude::plan_approval_drawer::plan_approval_drawer_height(area)
                .min(area.height.saturating_sub(reserved_height).max(1));
        let chunks = Layout::vertical([
            Constraint::Length(PLAN_APPROVAL_TOP_SPACER_HEIGHT),
            Constraint::Min(PLAN_APPROVAL_MIN_MESSAGES_HEIGHT),
            Constraint::Length(PLAN_APPROVAL_MESSAGE_GAP_HEIGHT),
            Constraint::Length(drawer_height),
        ])
        .split(area);
        state.geometry.messages_area = chunks[1];
        render_conversation(state, frame, chunks[1]);
        crate::ui::prelude::plan_approval_drawer::render_plan_approval_drawer(
            state, frame, chunks[3],
        );
        crate::ui::selection::apply_selection_overlay(state, frame.buffer_mut());
        return;
    }

    if focus == crate::app::focus::Focus::Pause {
        let reserved_height = TOOL_PAUSE_TOP_SPACER_HEIGHT
            + TOOL_PAUSE_MIN_MESSAGES_HEIGHT
            + TOOL_PAUSE_MESSAGE_GAP_HEIGHT
            + TOOL_PAUSE_FOOTER_HEIGHT;
        let drawer_height =
            crate::ui::prelude::permission_drawer::permission_drawer_height(state, area)
                .min(area.height.saturating_sub(reserved_height).max(1));
        let chunks = Layout::vertical([
            Constraint::Length(TOOL_PAUSE_TOP_SPACER_HEIGHT),
            Constraint::Min(TOOL_PAUSE_MIN_MESSAGES_HEIGHT),
            Constraint::Length(TOOL_PAUSE_MESSAGE_GAP_HEIGHT),
            Constraint::Length(drawer_height),
            Constraint::Length(TOOL_PAUSE_FOOTER_HEIGHT),
        ])
        .split(area);
        state.geometry.messages_area = chunks[1];
        render_conversation(state, frame, chunks[1]);
        crate::ui::prelude::status::render_footer(state, frame, chunks[4]);

        crate::ui::prelude::permission_drawer::render_permission_drawer(state, frame, chunks[3]);
        crate::ui::selection::apply_selection_overlay(state, frame.buffer_mut());
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
        state.geometry.messages_area = chunks[1];
        render_conversation(state, frame, chunks[1]);

        if state.dialogs.help_drawer.is_some() {
            crate::ui::prelude::help_drawer::render_help_drawer(state, frame, chunks[3]);
        } else if crate::ui::prelude::interactions::interaction_drawer_height(state, area).is_some()
        {
            crate::ui::prelude::interactions::render_interaction(state, frame, chunks[3]);
        }
        crate::ui::selection::apply_selection_overlay(state, frame.buffer_mut());
        return;
    }

    let drawer_len = state.composer.queued_user_inputs.len();
    let queued_height = if drawer_len == 0 {
        0
    } else {
        drawer_len.min(4) as u16 + 2
    };
    let input_height = 2 + state.composer.input_visible_line_count() as u16 + queued_height;
    let session_list_height = if state.sessions.subagent_order.is_empty() {
        0
    } else {
        state.session_count().min(u16::MAX as usize) as u16
    };
    let session_list_gap_height = u16::from(session_list_height > 0);
    let activity_height = u16::from(state.is_run_active() || state.background_wait_count() > 0);
    let activity_gap_height = if activity_height > 0 {
        MESSAGE_STATUS_GAP_HEIGHT
    } else {
        0
    };
    let input_gap_height = if activity_height > 0 {
        0
    } else {
        MESSAGE_INPUT_GAP_HEIGHT
    };
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(activity_gap_height),
        Constraint::Length(activity_height),
        Constraint::Length(input_gap_height),
        Constraint::Length(input_height),
        Constraint::Length(1),
        Constraint::Length(session_list_gap_height),
        Constraint::Length(session_list_height),
    ])
    .split(area);
    state.geometry.messages_area = chunks[1];

    render_conversation(state, frame, chunks[1]);
    render_activity(state, frame, chunks[3]);
    crate::ui::prelude::autocomplete::render_autocomplete(state, frame, chunks[5]);
    crate::ui::prelude::status::render_footer(state, frame, chunks[6]);
    render_session_selector(state, frame, chunks[8]);

    if state.dialogs.interaction_step.is_none()
        && state.active_tool_pause().is_none()
        && state.dialogs.help_drawer.is_none()
        && state.dialogs.plan.plan_approval.is_none()
    {
        crate::ui::prelude::input::render_input(state, frame, chunks[5]);
    }

    if state.dialogs.interaction_request.is_some() {
        crate::ui::prelude::interactions::render_interaction(state, frame, area);
    }

    crate::ui::prelude::help_drawer::render_help_drawer(state, frame, area);
    crate::ui::prelude::permission_drawer::render_permission_drawer(state, frame, area);
    crate::ui::selection::apply_selection_overlay(state, frame.buffer_mut());
}

fn render_session_selector(state: &ViewContext<'_>, frame: &mut ratatui::Frame, area: Rect) {
    if area.height == 0 || state.sessions.subagent_order.is_empty() {
        return;
    }
    let dim = Style::default().fg(crate::ui::theme::MUTED);
    let active_style = Style::default()
        .fg(crate::ui::theme::TEXT)
        .add_modifier(Modifier::BOLD);
    let selected_style = Style::default()
        .fg(crate::ui::theme::ACCENT)
        .add_modifier(Modifier::BOLD);
    let mut rows = Vec::with_capacity(state.session_count());
    rows.push(("main".to_string(), None, 0usize));
    for (index, task_id) in state.sessions.subagent_order.iter().enumerate() {
        let Some(node) = state
            .sessions
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
            let selected = if state.sessions.session_selector_focused {
                state.sessions.session_selection_index == index
            } else {
                state
                    .sessions
                    .active_session_task_id
                    .as_ref()
                    .is_some_and(|task_id| {
                        state.sessions.subagent_order.get(index.wrapping_sub(1)) == Some(task_id)
                    })
                    || (index == 0 && state.sessions.active_session_task_id.is_none())
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

fn should_render_start_screen(state: &ViewContext<'_>) -> bool {
    state.start.show_start_screen
        && state.sessions.active_session_task_id.is_none()
        && state.session.messages.is_empty()
        && state.session.pending_assistant.is_none()
        && state.session.pending_proposed_plan.is_none()
        && state.session.pending_compact_summary.is_none()
}

fn render_conversation(state: &mut ViewContext<'_>, frame: &mut ratatui::Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    if should_render_start_screen(state) {
        crate::ui::prelude::render_start_screen(state, frame, area);
    } else {
        crate::ui::prelude::render_messages(state, frame, area);
    }
}

fn bottom_drawer_height(state: &ViewContext<'_>, area: Rect) -> Option<u16> {
    if state.dialogs.help_drawer.is_some() {
        return Some(crate::ui::prelude::help_drawer::help_drawer_height(area));
    }
    crate::ui::prelude::interactions::interaction_drawer_height(state, area)
}

fn render_background(frame: &mut ratatui::Frame, area: Rect) {
    frame.render_widget(
        Paragraph::new(Line::from("")).style(
            Style::default()
                .fg(crate::ui::theme::TEXT)
                .bg(crate::ui::theme::BACKGROUND),
        ),
        area,
    );
}

fn render_activity(state: &mut ViewContext<'_>, frame: &mut ratatui::Frame, area: Rect) {
    if area.height == 0 {
        return;
    }

    let count = state.background_wait_count();
    if count > 0 {
        let noun = if count == 1 { "task" } else { "tasks" };
        let line = Line::styled(
            format!("• Waiting for {count} background {noun} to finish"),
            Style::default().fg(crate::ui::theme::MUTED),
        );
        crate::ui::prelude::register_selectable_lines(state, area, std::slice::from_ref(&line));
        frame.render_widget(Paragraph::new(line), area);
        return;
    }

    if !state.is_run_active() {
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
    let style = Style::default().fg(crate::ui::theme::MUTED);
    let bright = crate::ui::theme::MUTED;
    let dim = crate::ui::theme::BORDER;
    let label = match state.session.agent_status {
        AgentStatus::Thinking => "Thinking",
        AgentStatus::Working => "Working",
        AgentStatus::AwaitingInput => "Waiting for you",
        AgentStatus::Idle => return,
    };
    let elapsed = format_run_duration(elapsed);
    let meta = if state.session.agent_status == AgentStatus::AwaitingInput
        || state.is_run_timer_paused()
    {
        format!(" (paused at {elapsed})")
    } else {
        format!(" ({elapsed} · esc to interrupt)")
    };

    let mut spans = vec![Span::styled("• ", style)];
    if state.session.agent_status == AgentStatus::AwaitingInput {
        spans.push(Span::styled(label.to_string(), style));
    } else {
        spans.extend(
            crate::ui::prelude::status::animated_status_spans_with_palette(label, bright, dim),
        );
    }
    spans.push(Span::styled(meta, style));

    let line = Line::from(spans);
    crate::ui::prelude::register_selectable_lines(
        state,
        activity_area,
        std::slice::from_ref(&line),
    );
    frame.render_widget(Paragraph::new(line), activity_area);
}
