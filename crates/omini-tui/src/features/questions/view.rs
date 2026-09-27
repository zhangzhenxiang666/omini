use crate::ui::context::ViewContext;
use crate::ui::drawer::*;
use crate::ui::prelude::*;
pub fn build_user_input_action_lines(
    state: &ViewContext<'_>,
    _preview: &crate::app::event::UserInputPreview,
) -> Text<'static> {
    if state.note_mode() {
        Text::from(vec![Line::from(vec![
            Span::styled("Tab 或 Esc ", Style::default().fg(crate::ui::theme::MUTED)),
            Span::styled("结束备注", Style::default().fg(crate::ui::theme::MUTED)),
            Span::raw(" | "),
            Span::styled("Enter ", Style::default().fg(crate::ui::theme::MUTED)),
            Span::styled("提交回答", Style::default().fg(crate::ui::theme::MUTED)),
        ])])
    } else {
        Text::from(vec![Line::from(vec![
            Span::styled(
                "Tab 添加备注",
                Style::default()
                    .fg(crate::ui::theme::ACCENT)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" | "),
            Span::styled(
                "Enter 提交回答",
                Style::default().fg(crate::ui::theme::MUTED),
            ),
            Span::raw(" | "),
            Span::styled("←/→ 切换问题", Style::default().fg(crate::ui::theme::MUTED)),
            Span::raw(" | "),
            Span::styled("Esc 中断", Style::default().fg(crate::ui::theme::MUTED)),
        ])])
    }
}

pub fn user_input_question_lines(
    question: &crate::app::event::UserInputQuestion,
    content_width: usize,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(Span::styled(
        question.header.clone(),
        Style::default().fg(crate::ui::theme::MUTED),
    ))];
    lines.extend(
        crate::features::tools::word_wrap(&question.question, content_width)
            .into_iter()
            .map(|line| {
                Line::from(Span::styled(
                    line,
                    Style::default()
                        .fg(crate::ui::theme::ACCENT)
                        .add_modifier(Modifier::BOLD),
                ))
            }),
    );
    lines
}

pub fn user_input_option_line(
    selected: bool,
    label: &str,
    description: &str,
    label_width: usize,
) -> Line<'static> {
    let marker_style = if selected {
        Style::default()
            .fg(crate::ui::theme::ACCENT)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(crate::ui::theme::BORDER)
    };
    let label_style = if selected {
        Style::default()
            .fg(crate::ui::theme::ACCENT)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(crate::ui::theme::TEXT)
    };
    Line::from(vec![
        Span::styled(if selected { "› " } else { "  " }, marker_style),
        Span::styled(pad_display_width(label, label_width), label_style),
        Span::raw("   "),
        Span::styled(
            description.to_string(),
            Style::default().fg(crate::ui::theme::MUTED),
        ),
    ])
}

pub fn build_question_drawer(
    input: PermissionDrawerLinesInput<'_>,
    preview: &crate::app::event::UserInputPreview,
) -> DrawerLines {
    let PermissionDrawerLinesInput {
        content_width,
        question_index,
        user_input_selected,
        current_user_input_note,
        user_input_note_cursor,
        user_input_note_mode,
        ..
    } = input;

    let Some(question) = preview.questions.get(question_index) else {
        return DrawerLines {
            lines: vec![Line::from("缺少问题")],
            note_lines: Vec::new(),
            note_cursor: None,
        };
    };
    let mut lines = Vec::new();
    lines.extend(user_input_question_lines(question, content_width));
    lines.push(Line::from(""));
    for (index, (label, description)) in question
        .options
        .iter()
        .map(|option| (option.label.as_str(), option.description.as_str()))
        .chain(std::iter::once((
            USER_INPUT_NONE_LABEL,
            USER_INPUT_NONE_DESCRIPTION,
        )))
        .enumerate()
    {
        let selected = index == user_input_selected;
        let marker = if selected { "›" } else { " " };
        let style = Style::default().fg(if selected {
            crate::ui::theme::ACCENT
        } else {
            crate::ui::theme::TEXT
        });
        for (row, text) in crate::features::tools::word_wrap(
            &format!("{marker} {}. {label}", index + 1),
            content_width.max(1),
        )
        .into_iter()
        .enumerate()
        {
            let _ = row;
            lines.push(Line::from(Span::styled(text, style)));
        }
        for text in
            crate::features::tools::word_wrap(description, content_width.saturating_sub(3).max(1))
        {
            lines.push(Line::from(Span::styled(
                format!("   {text}"),
                Style::default().fg(crate::ui::theme::MUTED),
            )));
        }
    }
    let mut drawer = DrawerLines {
        lines,
        note_lines: Vec::new(),
        note_cursor: None,
    };
    set_note_line(
        &mut drawer,
        current_user_input_note,
        user_input_note_cursor,
        user_input_note_mode,
        false,
        content_width,
    );
    drawer
}
