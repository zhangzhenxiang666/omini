use super::*;

pub(super) fn plan_approval_drawer_height(area: Rect) -> u16 {
    area.height.min(8)
}

pub(super) fn render_plan_approval_drawer(
    state: &mut UiState,
    frame: &mut ratatui::Frame,
    area: Rect,
) {
    if state.plan_approval.is_none() {
        return;
    }
    if area.width == 0 || area.height == 0 {
        return;
    }

    let drawer_area = area;

    let panel_height = drawer_area.height.saturating_sub(1);
    let panel_area = Rect {
        height: panel_height,
        ..drawer_area
    };

    if panel_area.height > 0 {
        let bg = Style::default().bg(INPUT_BG);
        frame.render_widget(Paragraph::new(Line::from("")).style(bg), panel_area);

        let mut lines = build_panel_lines(state.plan_approval_selected, panel_area.height).lines;
        register_and_highlight_lines(state, panel_area, &mut lines);
        frame.render_widget(Paragraph::new(lines).style(bg), panel_area);
    }

    let footer_area = Rect {
        x: drawer_area.x,
        y: drawer_area.y + drawer_area.height.saturating_sub(1),
        width: drawer_area.width,
        height: 1,
    };
    let mut footer = build_footer_line();
    register_and_highlight_lines(state, footer_area, std::slice::from_mut(&mut footer));
    frame.render_widget(Paragraph::new(footer), footer_area);
}

fn build_panel_lines(selected: usize, height: u16) -> Text<'static> {
    let mut lines = Vec::new();
    let bg = Style::default().bg(INPUT_BG);

    if height >= 6 {
        lines.push(Line::from(Span::styled("", bg)));
    }

    lines.push(Line::from(vec![
        Span::styled("  ", bg),
        Span::styled(
            "Implement this plan?",
            Style::default()
                .fg(Color::Rgb(220, 220, 225))
                .bg(INPUT_BG)
                .add_modifier(Modifier::BOLD),
        ),
    ]));

    if height >= 5 {
        lines.push(Line::from(Span::styled("", bg)));
    }

    lines.extend(build_action_lines(selected));
    if height >= 7 {
        lines.push(Line::from(Span::styled("", bg)));
    }
    lines.truncate(height as usize);
    Text::from(lines)
}

fn build_footer_line() -> Line<'static> {
    let hint_style = Style::default().fg(Color::Rgb(140, 145, 155));
    Line::from(vec![
        Span::raw("  "),
        Span::styled("Enter", hint_style),
        Span::styled(" 确认当前选项，", hint_style),
        Span::styled("Esc", hint_style),
        Span::styled(" 返回继续讨论", hint_style),
    ])
}

fn build_action_lines(selected: usize) -> Vec<Line<'static>> {
    let actions = [
        ("Yes, implement this plan", "切换到 Default 并开始编码。"),
        (
            "Yes, clear context and implement",
            "开启新上下文，仅保留计划内容。",
        ),
        ("No, stay in Plan mode", "继续和模型讨论计划。"),
    ];
    actions
        .iter()
        .enumerate()
        .map(|(idx, (label, desc))| {
            let selected_style = if idx == selected {
                Style::default()
                    .fg(Color::Rgb(0x42, 0xd9, 0xe8))
                    .bg(INPUT_BG)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Rgb(220, 220, 225)).bg(INPUT_BG)
            };
            let gutter = if idx == selected { "› " } else { "  " };
            Line::from(vec![
                Span::styled(
                    gutter,
                    Style::default()
                        .fg(Color::Rgb(0x42, 0xd9, 0xe8))
                        .bg(INPUT_BG)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("{}. {:<34}", idx + 1, label), selected_style),
                Span::styled(
                    *desc,
                    Style::default().fg(Color::Rgb(140, 145, 155)).bg(INPUT_BG),
                ),
            ])
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selection::selected_text;
    use crate::state::{SelectionPoint, TextSelection};
    use crate::types::display::DisplayPlan;
    use chrono::Utc;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    #[test]
    fn plan_approval_actions_use_english_labels_and_chinese_descriptions() {
        let lines = build_action_lines(1);
        let rendered = lines.iter().map(line_to_plain_text).collect::<Vec<_>>();

        assert!(rendered[0].starts_with("  1."));
        assert!(rendered[0].contains("Yes, implement this plan"));
        assert!(rendered[0].contains("切换到 Default 并开始编码。"));
        assert!(rendered[1].starts_with("› 2."));
        assert!(rendered[1].contains("Yes, clear context and implement"));
        assert!(rendered[1].contains("开启新上下文，仅保留计划内容。"));
        assert!(rendered[2].starts_with("  3."));
        assert!(rendered[2].contains("No, stay in Plan mode"));
        assert!(rendered[2].contains("继续和模型讨论计划。"));
    }

    #[test]
    fn plan_approval_panel_lines_keep_footer_outside_panel() {
        let text = build_panel_lines(0, 7);
        let rendered = text
            .lines
            .iter()
            .map(line_to_plain_text)
            .collect::<Vec<_>>();

        assert!(rendered.iter().any(|line| line == "  Implement this plan?"));
        assert!(
            !rendered
                .iter()
                .any(|line| line.contains("Enter 确认当前选项"))
        );
        assert!(
            text.lines
                .iter()
                .flat_map(|line| line.spans.iter())
                .all(|span| span.style.bg == Some(INPUT_BG))
        );
        assert_eq!(
            text.lines.last().map(line_to_plain_text),
            Some(String::new())
        );
    }

    #[test]
    fn plan_approval_footer_hint_has_no_panel_background() {
        let footer = build_footer_line();
        let rendered = line_to_plain_text(&footer);

        assert!(rendered.starts_with("  Enter"));
        assert!(rendered.contains("Enter 确认当前选项，Esc 返回继续讨论"));
        assert!(
            footer
                .spans
                .iter()
                .skip(1)
                .all(|span| span.style.fg == Some(Color::Rgb(140, 145, 155)))
        );
        assert!(footer.spans.iter().all(|span| span.style.bg.is_none()));
    }

    #[test]
    fn plan_approval_drawer_registers_lines_for_mouse_selection() {
        let backend = TestBackend::new(80, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = UiState::new();
        state.plan_approval = Some(DisplayPlan {
            id: "preview-plan".to_string(),
            title: "Plan".to_string(),
            markdown: "# Plan".to_string(),
            path: "/tmp/plan.md".into(),
            created_at: Utc::now(),
        });

        terminal
            .draw(|frame| {
                state.clear_selectable_screen_lines();
                render_plan_approval_drawer(&mut state, frame, frame.area());
            })
            .unwrap();

        let title_line = state
            .selectable_screen_lines
            .iter()
            .find(|line| line.text.contains("Implement this plan?"))
            .expect("title line should be selectable")
            .clone();
        let footer_line = state
            .selectable_screen_lines
            .iter()
            .find(|line| line.text.contains("Enter 确认当前选项"))
            .expect("footer line should be selectable")
            .clone();

        state.text_selection = Some(TextSelection {
            start: SelectionPoint {
                row: title_line.row as usize,
                col: 2,
            },
            end: SelectionPoint {
                row: footer_line.row as usize,
                col: 6,
            },
        });

        let selected = selected_text(&state).expect("drawer text should be selectable");
        assert!(selected.contains("Implement this plan?"));
        assert!(selected.contains("Yes, implement this plan"));
        assert!(selected.contains("Enter"));
    }
}
