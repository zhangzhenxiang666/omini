use crate::ui::context::ViewContext;
use crate::ui::drawer::wrap_preserving_display_width;
use crate::ui::text::register_selectable_lines;
use crate::ui::theme;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

const STOP_QUESTION: &str = "是否停止当前运行及全部后台任务（含子任务）？";
const STOP_HINT: &str = "↑↓/Tab 选择 · Enter 确认 · Esc 返回";

/// 按提示的实际换行预留抽屉高度，正常尺寸留出正文与选项之间的空白。
pub fn stop_drawer_height(area: Rect) -> u16 {
    let width = content_width(area.width);
    let question_height = wrap_preserving_display_width(STOP_QUESTION, width).len();
    let hint_height = wrap_preserving_display_width(STOP_HINT, width).len();
    (question_height + hint_height + 6).min(area.height as usize) as u16
}

/// 只渲染布局分配的底部区域；后台事件继续更新，确认前不改变运行状态。
pub fn render_stop_drawer(state: &mut ViewContext<'_>, frame: &mut ratatui::Frame, area: Rect) {
    let Some(confirmation) = &state.dialogs.stop_confirmation else {
        return;
    };
    if area.width == 0 || area.height == 0 {
        return;
    }
    // 沿用权限抽屉的顶部细线；极短终端优先保留选项与按键提示。
    let divider_height = u16::from(area.height > 3);
    if divider_height > 0 {
        frame.render_widget(
            Paragraph::new(Line::styled(
                "─".repeat(area.width.saturating_sub(1) as usize),
                Style::default().fg(theme::ACCENT).bg(theme::BACKGROUND),
            )),
            Rect {
                height: divider_height,
                ..area
            },
        );
    }
    let panel_area = Rect {
        y: area.y + divider_height,
        height: area.height - divider_height,
        ..area
    };
    let lines = build_drawer_lines(confirmation.stop_selected, area.width, panel_area.height);
    theme::clear_panel(frame, panel_area);
    let padding = (area.width as usize - content_width(area.width)) as u16 / 2;
    let content_area = Rect {
        x: area.x + padding,
        width: area.width.saturating_sub(padding * 2),
        ..panel_area
    };
    register_selectable_lines(state, content_area, &lines);
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().fg(theme::TEXT).bg(theme::PANEL)),
        content_area,
    );
}

fn content_width(width: u16) -> usize {
    // 极窄终端省去左右内边距，把可用列留给选项与按键。
    width.saturating_sub(if width >= 20 { 4 } else { 0 }) as usize
}

/// 高度不足时先压缩空白，再缩短提示和说明，始终优先呈现可选操作。
fn build_drawer_lines(stop_selected: bool, width: u16, height: u16) -> Vec<Line<'static>> {
    let choice = |label: &'static str, selected: bool| {
        let style = if selected {
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::TEXT)
        };
        Line::from(vec![
            Span::styled(if selected { "❯ " } else { "  " }, style),
            Span::styled(label, style),
        ])
    };
    if height < 2 {
        return vec![choice(if stop_selected { "是" } else { "否" }, true)];
    }
    let width = content_width(width);
    let available = height as usize - 2;
    let mut question = wrap_preserving_display_width(STOP_QUESTION, width);
    let mut hints = wrap_preserving_display_width(STOP_HINT, width);
    if question.len() + hints.len() > available {
        let hint = if width >= 15 {
            "↑↓/Tab Enter Esc"
        } else {
            "Enter Esc"
        };
        hints = wrap_preserving_display_width(hint, width);
    }
    hints.truncate(available);
    question.truncate(available.saturating_sub(hints.len()));
    let spare = available - question.len() - hints.len();
    let mut lines = Vec::new();
    lines.extend(
        question
            .into_iter()
            .map(|text| Line::styled(text, Style::default().add_modifier(Modifier::BOLD))),
    );
    if spare >= 1 {
        lines.push(Line::default());
    }
    lines.push(choice("是", stop_selected));
    lines.push(choice("否", !stop_selected));
    if spare >= 2 {
        lines.push(Line::default());
    }
    lines.extend(
        hints
            .into_iter()
            .map(|text| Line::styled(text, Style::default().fg(theme::MUTED))),
    );
    if spare >= 3 {
        lines.push(Line::default());
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::{AppState, UiMessage};
    use crate::features::dialogs::state::StopConfirmation;
    use omini_model::message::{ContentBlock, TextBlock};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// 抽屉占据底部全宽，保留上方对话；两种选择都以中文短选项呈现。
    #[test]
    fn render_confirmation() {
        for (width, height) in [(120, 36), (80, 24), (40, 12)] {
            for stop_selected in [false, true] {
                let mut state = AppState::new();
                state.start.show_start_screen = false;
                state.sessions.views["main"]
                    .messages
                    .extend(UiMessage::from_blocks(vec![ContentBlock::Text(
                        TextBlock {
                            text: "保留对话".into(),
                        },
                    )]));
                state.dialogs.stop_confirmation = Some(StopConfirmation { stop_selected });
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                terminal
                    .draw(|frame| crate::app::draw(&mut state, frame))
                    .unwrap();
                let buffer = terminal.backend().buffer();
                let screen = buffer
                    .content()
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect::<String>()
                    .chars()
                    .filter(|character| !character.is_whitespace())
                    .collect::<String>();
                assert!(screen.contains(STOP_QUESTION));
                assert!(screen.contains("保留对话"));
                assert!(screen.contains(if stop_selected { "❯是" } else { "❯否" }));
                assert!(!screen.contains("不停止"));
                assert!(screen.contains("Esc"));
                assert!(screen.contains("返回"));
                let drawer_top = height - stop_drawer_height(Rect::new(0, 0, width, height));
                assert!(state.geometry.messages_area.bottom() <= drawer_top);
                assert_eq!(buffer[(0, drawer_top)].symbol(), "─");
                assert_eq!(buffer[(0, drawer_top)].fg, theme::ACCENT);
                assert_eq!(buffer[(0, drawer_top)].bg, theme::BACKGROUND);
                assert_eq!(buffer[(width - 2, drawer_top)].symbol(), "─");
                if height >= stop_drawer_height(Rect::new(0, 0, width, height)) + 3 {
                    assert_eq!(state.geometry.messages_area.bottom() + 1, drawer_top);
                }
                for y in drawer_top + 1..height {
                    assert_eq!(buffer[(0, y)].bg, theme::PANEL);
                    assert_eq!(buffer[(width - 1, y)].bg, theme::PANEL);
                }
                assert!(buffer.content().iter().any(|cell| {
                    cell.symbol() == (if stop_selected { "是" } else { "否" })
                        && cell.fg == theme::ACCENT
                }));
            }
        }
    }

    /// 高度不足时保留两个选项和简短按键提示，极小尺寸也不越界。
    #[test]
    fn render_tiny_confirmation() {
        for (width, height) in [(12, 4), (80, 4), (1, 1), (0, 0)] {
            let mut state = AppState::new();
            state.dialogs.stop_confirmation = Some(Default::default());
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| crate::app::draw(&mut state, frame))
                .unwrap();
            if height >= 4 {
                let screen = terminal
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect::<String>();
                assert!(screen.contains("是"));
                assert!(screen.contains("❯ 否"));
                assert!(screen.contains("Enter"));
                assert!(screen.contains("Esc"));
            }
        }
    }
}
