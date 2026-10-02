use crate::app::state::AppState;
use crate::ui::theme;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Padding, Paragraph, Wrap};

/// 停止确认覆盖当前视图；后台事件仍正常更新，渲染不改变运行状态。
pub fn render_stop_confirmation(state: &AppState, frame: &mut ratatui::Frame) {
    let Some(confirmation) = &state.dialogs.stop_confirmation else {
        return;
    };
    let screen = frame.area();
    let width = screen.width.min(60);
    let height = screen.height.min(11);
    let area = Rect::new(
        screen.x + screen.width.saturating_sub(width) / 2,
        screen.y + screen.height.saturating_sub(height) / 2,
        width,
        height,
    );
    theme::clear_panel(frame, area);
    let choice = |label: &'static str, selected: bool| {
        Line::from(vec![
            Span::raw(if selected { "❯ " } else { "  " }),
            Span::styled(
                label,
                if selected {
                    Style::default()
                        .fg(theme::ACCENT)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme::TEXT)
                },
            ),
        ])
    };
    let lines = vec![
        Line::from("有后台任务。是否停止当前运行及全部后台任务（含子任务）？"),
        Line::default(),
        choice("停止", confirmation.stop_selected),
        choice("不停止", !confirmation.stop_selected),
        Line::default(),
        Line::styled(
            "↑↓/Tab 选择 · Enter 确认 · Esc 返回",
            Style::default().fg(theme::MUTED),
        ),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .style(Style::default().fg(theme::TEXT).bg(theme::PANEL))
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .title("确认停止")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(theme::BORDER))
                    .padding(Padding::horizontal(1)),
            ),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// 大小终端均展示停止范围与操作；默认高亮“不停止”。
    #[test]
    fn render_confirmation() {
        for (width, height) in [(120, 36), (80, 24), (40, 12), (12, 4)] {
            let mut state = AppState::new();
            state.dialogs.stop_confirmation = Some(Default::default());
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| crate::app::draw(&mut state, frame))
                .unwrap();
            if width < 40 {
                continue;
            }
            let buffer = terminal.backend().buffer();
            let screen = buffer
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>()
                .chars()
                .filter(|character| !character.is_whitespace())
                .collect::<String>();
            assert!(screen.contains("确认停止"));
            assert!(screen.contains("后台任务"));
            assert!(screen.contains("❯不停止"));
            assert!(screen.contains("Esc"));
            assert!(screen.contains("返回"));
            assert!(
                buffer
                    .content()
                    .iter()
                    .any(|cell| { cell.symbol() == "不" && cell.fg == theme::ACCENT })
            );
        }
    }
}
