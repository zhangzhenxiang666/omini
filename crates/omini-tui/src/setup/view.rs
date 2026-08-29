use crate::setup::ConfigurationForm;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

const SETUP_BACKGROUND: Color = Color::Rgb(40, 44, 52);
const SETUP_INPUT_BACKGROUND: Color = Color::Rgb(65, 69, 76);
const SETUP_ACCENT: Color = Color::Rgb(0x42, 0xd9, 0xe8);
const SETUP_BORDER: Color = Color::Rgb(0x35, 0x8c, 0x98);
const SETUP_TEXT: Color = Color::Rgb(0xc6, 0xd0, 0xdc);
const SETUP_MUTED: Color = Color::Rgb(0x7a, 0x82, 0x8e);
const SETUP_ERROR: Color = Color::Rgb(255, 100, 100);

pub(super) fn render_form(frame: &mut ratatui::Frame, form: &ConfigurationForm) {
    let area = frame.area();
    render_setup_background(frame, area);
    let compact = area.width < 72 || area.height < 23;
    let panel_area = centered_setup_panel(area, if compact { 16 } else { 21 });
    let panel = setup_panel(" omini ");
    let inner = panel.inner(panel_area);
    frame.render_widget(panel, panel_area);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let mut y = inner.y;
    render_setup_line(
        frame,
        inner,
        y,
        Line::from(Span::styled(
            "Connect a model provider",
            Style::default()
                .fg(SETUP_ACCENT)
                .add_modifier(Modifier::BOLD),
        )),
    );
    y = y.saturating_add(1);
    if !compact {
        render_setup_line(
            frame,
            inner,
            y,
            Line::from(Span::styled(
                "Add the minimum configuration required to start omini.",
                Style::default().fg(SETUP_MUTED),
            )),
        );
        y = y.saturating_add(1);
    }
    render_setup_divider(frame, inner, y);
    y = y.saturating_add(1);

    let api_key = if form.api_key.is_empty() {
        String::new()
    } else {
        "•".repeat(form.api_key.chars().count())
    };
    let fields = [
        ("Protocol", String::new()),
        ("Provider ID", form.provider_id.clone()),
        ("Base URL", form.base_url.clone()),
        ("Model ID", form.model_id.clone()),
        ("Environment variable", form.environment_variable.clone()),
        ("API key", api_key),
    ];
    let label_width = if inner.width >= 68 { 22 } else { 15 };
    let mut cursor = None;
    for (index, (label, value)) in fields.into_iter().enumerate() {
        let row = setup_content_row(inner, y);
        if row.height > 0 {
            render_configuration_field(frame, row, form, index, label, &value, label_width);
            if form.selected == index && (1..=5).contains(&index) {
                let cursor_width = if index == 5 {
                    form.cursor
                } else {
                    UnicodeWidthStr::width(
                        value.chars().take(form.cursor).collect::<String>().as_str(),
                    )
                };
                let x = row
                    .x
                    .saturating_add(2 + label_width as u16 + 1)
                    .saturating_add(cursor_width as u16)
                    .min(row.x.saturating_add(row.width.saturating_sub(1)));
                cursor = Some((x, row.y));
            }
        }
        y = y.saturating_add(1);
    }

    y = y.saturating_add(1);
    if !compact {
        render_setup_line(
            frame,
            inner,
            y,
            Line::from(vec![
                Span::styled("Local only  ", Style::default().fg(SETUP_ACCENT)),
                Span::styled(
                    "API keys are stored in ~/.omini/auth.json with 0600 permissions.",
                    Style::default().fg(SETUP_MUTED),
                ),
            ]),
        );
        y = y.saturating_add(2);
    }

    let button = setup_content_row(inner, y);
    render_setup_button(frame, button, form.selected == 6);
    y = y.saturating_add(2);

    if let Some(error) = &form.error {
        let error_area = Rect {
            x: inner.x.saturating_add(2),
            y,
            width: inner.width.saturating_sub(4),
            height: inner.bottom().saturating_sub(y),
        };
        if error_area.height > 0 {
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("Error  ", Style::default().fg(SETUP_ERROR)),
                    Span::styled(error.clone(), Style::default().fg(SETUP_TEXT)),
                ]))
                .wrap(Wrap { trim: true }),
                error_area,
            );
        }
    }

    render_setup_footer(
        frame,
        area,
        &[
            ("↑↓/Tab", "navigate"),
            ("←→", "protocol"),
            ("Enter", "continue"),
            ("Esc", "quit"),
        ],
    );
    if let Some(position) = cursor {
        frame.set_cursor_position(position);
    }
}

pub(super) fn render_invalid(frame: &mut ratatui::Frame, message: &str) {
    let area = frame.area();
    render_setup_background(frame, area);
    let panel_area = centered_setup_panel(area, 16);
    let panel = setup_panel(" omini ");
    let inner = panel.inner(panel_area);
    frame.render_widget(panel, panel_area);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    if inner.height < 8 {
        render_setup_line(
            frame,
            inner,
            inner.y,
            Line::from(Span::styled(
                "Configuration needs manual repair",
                Style::default().fg(SETUP_ERROR),
            )),
        );
        render_setup_line(
            frame,
            inner,
            inner.y.saturating_add(1),
            Line::from(Span::styled(
                message.to_string(),
                Style::default().fg(SETUP_TEXT),
            )),
        );
        render_setup_footer(frame, area, &[("Esc/q", "quit")]);
        return;
    }

    render_setup_line(
        frame,
        inner,
        inner.y,
        Line::from(vec![
            Span::styled(
                "Configuration needs attention",
                Style::default()
                    .fg(SETUP_ERROR)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("  manual repair required", Style::default().fg(SETUP_MUTED)),
        ]),
    );
    render_setup_divider(frame, inner, inner.y.saturating_add(2));

    let message_area = Rect {
        x: inner.x.saturating_add(2),
        y: inner.y.saturating_add(4),
        width: inner.width.saturating_sub(4),
        height: 3.min(inner.height.saturating_sub(4)),
    };
    frame.render_widget(
        Paragraph::new(message.to_string())
            .style(Style::default().fg(SETUP_TEXT))
            .wrap(Wrap { trim: true }),
        message_area,
    );

    let repair_y = message_area.bottom().saturating_add(1);
    let repair_area = Rect {
        x: inner.x.saturating_add(2),
        y: repair_y,
        width: inner.width.saturating_sub(4),
        height: inner.bottom().saturating_sub(repair_y),
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                "Repair one of these files, then restart omini:",
                Style::default().fg(SETUP_MUTED),
            )),
            Line::from(vec![
                Span::styled("  config  ", Style::default().fg(SETUP_ACCENT)),
                Span::styled("~/.omini/config.toml", Style::default().fg(SETUP_TEXT)),
            ]),
            Line::from(vec![
                Span::styled("  auth    ", Style::default().fg(SETUP_ACCENT)),
                Span::styled("~/.omini/auth.json", Style::default().fg(SETUP_TEXT)),
            ]),
        ]),
        repair_area,
    );
    render_setup_footer(frame, area, &[("Esc/q", "quit")]);
}

fn render_setup_background(frame: &mut ratatui::Frame, area: Rect) {
    frame.render_widget(
        Block::default().style(Style::default().bg(SETUP_BACKGROUND)),
        area,
    );
}

fn centered_setup_panel(area: Rect, desired_height: u16) -> Rect {
    let footer_height = u16::from(area.height > 1);
    let available_height = area.height.saturating_sub(footer_height);
    let width = area.width.saturating_sub(2).min(92);
    let height = desired_height.min(available_height);
    Rect {
        x: area.x.saturating_add(area.width.saturating_sub(width) / 2),
        y: area
            .y
            .saturating_add(available_height.saturating_sub(height) / 2),
        width,
        height,
    }
}

fn setup_panel(title: &'static str) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(SETUP_BORDER))
        .title(Span::styled(
            title,
            Style::default()
                .fg(SETUP_ACCENT)
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(SETUP_BACKGROUND))
}

fn setup_content_row(inner: Rect, y: u16) -> Rect {
    Rect {
        x: inner.x.saturating_add(2),
        y,
        width: inner.width.saturating_sub(4),
        height: u16::from(y < inner.bottom()),
    }
}

fn render_setup_line(frame: &mut ratatui::Frame, inner: Rect, y: u16, line: Line<'static>) {
    let area = setup_content_row(inner, y);
    if area.height > 0 {
        frame.render_widget(Paragraph::new(line), area);
    }
}

fn render_setup_divider(frame: &mut ratatui::Frame, inner: Rect, y: u16) {
    let area = setup_content_row(inner, y);
    if area.height > 0 {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "─".repeat(area.width as usize),
                Style::default().fg(SETUP_BORDER),
            ))),
            area,
        );
    }
}

fn render_configuration_field(
    frame: &mut ratatui::Frame,
    area: Rect,
    form: &ConfigurationForm,
    index: usize,
    label: &str,
    value: &str,
    label_width: usize,
) {
    let selected = form.selected == index;
    let row_background = if selected {
        SETUP_INPUT_BACKGROUND
    } else {
        SETUP_BACKGROUND
    };
    let label_style = Style::default()
        .fg(if selected { SETUP_ACCENT } else { SETUP_MUTED })
        .bg(row_background)
        .add_modifier(if selected {
            Modifier::BOLD
        } else {
            Modifier::empty()
        });
    let value_style = Style::default().fg(SETUP_TEXT).bg(row_background);
    let mut spans = vec![
        Span::styled(if selected { "› " } else { "  " }, label_style),
        Span::styled(format!("{label:<label_width$}"), label_style),
        Span::styled(" ", value_style),
    ];
    if index == 0 {
        let openai = form.protocol == omini_protocol::ProviderEndpointKind::OpenAI;
        spans.extend([
            Span::styled(
                if openai {
                    "● OpenAI compatible"
                } else {
                    "○ OpenAI compatible"
                },
                Style::default()
                    .fg(if openai { SETUP_ACCENT } else { SETUP_MUTED })
                    .bg(row_background),
            ),
            Span::styled("   ", value_style),
            Span::styled(
                if openai {
                    "○ Anthropic"
                } else {
                    "● Anthropic"
                },
                Style::default()
                    .fg(if openai { SETUP_MUTED } else { SETUP_ACCENT })
                    .bg(row_background),
            ),
        ]);
    } else if index == 5 && value.is_empty() {
        spans.push(Span::styled(
            "optional for keyless providers",
            Style::default()
                .fg(SETUP_MUTED)
                .bg(row_background)
                .add_modifier(Modifier::DIM),
        ));
    } else {
        spans.push(Span::styled(value.to_string(), value_style));
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(row_background)),
        area,
    );
}

fn render_setup_button(frame: &mut ratatui::Frame, area: Rect, selected: bool) {
    if area.height == 0 {
        return;
    }
    let style = if selected {
        Style::default()
            .fg(SETUP_BACKGROUND)
            .bg(SETUP_ACCENT)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(SETUP_ACCENT)
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            if selected {
                "  Save and continue  "
            } else {
                "[ Save and continue ]"
            },
            style,
        )))
        .alignment(Alignment::Center),
        area,
    );
}

fn render_setup_footer(
    frame: &mut ratatui::Frame,
    area: Rect,
    actions: &[(&'static str, &'static str)],
) {
    if area.height == 0 {
        return;
    }
    let footer = Rect {
        x: area.x,
        y: area.bottom().saturating_sub(1),
        width: area.width,
        height: 1,
    };
    let mut spans = Vec::new();
    for (index, (key, action)) in actions.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled("   ", Style::default().fg(SETUP_MUTED)));
        }
        spans.push(Span::styled(*key, Style::default().fg(SETUP_ACCENT)));
        spans.push(Span::styled(
            format!("  {action}"),
            Style::default().fg(SETUP_MUTED),
        ));
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)).alignment(Alignment::Center),
        footer,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use omini_protocol::{ProjectConfigurationResponse, ProjectConfigurationState};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn form() -> ConfigurationForm {
        ConfigurationForm::new(&ProjectConfigurationResponse {
            state: ProjectConfigurationState::SetupRequired,
            code: Some("no_provider".to_string()),
            message: None,
            provider_id: None,
        })
    }

    fn rendered(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn setup_form_uses_the_main_tui_visual_language() {
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| render_form(frame, &form())).unwrap();

        let output = rendered(&terminal);
        assert!(output.contains("Connect a model provider"));
        assert!(output.contains("● OpenAI compatible"));
        assert!(output.contains("OPENAI_API_KEY"));
        assert!(output.contains("Save and continue"));
        assert!(output.contains("Local only"));
        assert!(output.contains("› Protocol"));
    }

    #[test]
    fn setup_form_never_renders_the_api_key() {
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut form = form();
        form.selected = 5;
        form.api_key = "top-secret".to_string();

        terminal.draw(|frame| render_form(frame, &form)).unwrap();

        let output = rendered(&terminal);
        assert!(!output.contains("top-secret"));
        assert!(output.contains("••••••••••"));
    }

    #[test]
    fn setup_form_places_the_terminal_cursor_at_the_text_cursor() {
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut form = form();
        form.select(3);

        terminal.draw(|frame| render_form(frame, &form)).unwrap();
        let end = terminal.get_cursor_position().unwrap();
        form.move_cursor_left();
        terminal.draw(|frame| render_form(frame, &form)).unwrap();
        let moved = terminal.get_cursor_position().unwrap();

        assert_eq!(end.y, moved.y);
        assert_eq!(end.x, moved.x + 1);
    }

    #[test]
    fn setup_views_render_in_a_small_terminal() {
        let backend = TestBackend::new(36, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|frame| render_form(frame, &form())).unwrap();
        terminal
            .draw(|frame| render_invalid(frame, "invalid config"))
            .unwrap();
    }
}
