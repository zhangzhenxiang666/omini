use super::*;

pub(super) fn render_interaction(state: &mut UiState, frame: &mut ratatui::Frame, area: Rect) {
    if let Some(InteractionStep::Agents(manager)) = &mut state.interaction_step {
        manager.set_draft_wrap_width(super::agents::input_box_inner_width(
            area.width.saturating_sub(4) as usize,
        ));
    }

    if let Some(InteractionStep::Agents(manager)) = state.interaction_step.clone() {
        super::agents::render_agents_panel(state, frame, area, &manager);
        return;
    }

    // ThinkingEffort is now inlined inside ModelSelection
    let Some(InteractionStep::ModelSelection {
        entries,
        selected,
        thinking_idx,
        active_provider,
        active_model,
    }) = state.interaction_step.clone()
    else {
        return;
    };

    // Panel height
    let has_thinking = entries
        .get(selected)
        .is_some_and(|e| matches!(e, ModelSelectionEntry::Model { model, .. } if model.thinking));
    // title(1) + subtitle(1) + divider(1) + entries + gap(0-1) + thinking(0-1) + hint(1)
    let extra: u16 = if has_thinking { 6 } else { 4 };
    let panel_height = ((entries.len() as u16) + extra)
        .clamp(5, 22)
        .min(area.height.saturating_sub(4).max(1));

    let panel_area = Rect {
        x: area.x,
        y: area.y + area.height - panel_height,
        width: area.width,
        height: panel_height,
    };

    // Clear only — no background color
    frame.render_widget(Clear, panel_area);

    // ── Header: title + subtitle + thick divider ──
    let accent = Color::Rgb(0x42, 0xd9, 0xe8);

    // Line 0: thick divider above the panel (━ characters, accent color)
    let mut divider_line = Line::from(Span::styled(
        "━".repeat(panel_area.width.saturating_sub(1) as usize),
        Style::default().fg(accent),
    ));
    let divider_area = Rect {
        x: panel_area.x,
        y: panel_area.y.saturating_sub(1),
        width: panel_area.width,
        height: 1,
    };
    register_and_highlight_lines(state, divider_area, std::slice::from_mut(&mut divider_line));

    frame.render_widget(Paragraph::new(divider_line), divider_area);

    // Line 1: "Select model" in accent color, bold
    let mut title_line = Line::from(Span::styled(
        " Select model",
        Style::default().fg(accent).add_modifier(Modifier::BOLD),
    ));
    let title_area = Rect {
        x: panel_area.x,
        y: panel_area.y + 1,
        width: panel_area.width,
        height: 1,
    };
    register_and_highlight_lines(state, title_area, std::slice::from_mut(&mut title_line));

    frame.render_widget(Paragraph::new(title_line), title_area);

    // Line 2: Chinese subtitle in gray
    let mut subtitle_line = Line::from(Span::styled(
        " 切换模型，适用于当前会话和未来会话。",
        Style::default().fg(Color::Rgb(140, 145, 155)),
    ));
    let subtitle_area = Rect {
        x: panel_area.x,
        y: panel_area.y + 2,
        width: panel_area.width,
        height: 1,
    };
    register_and_highlight_lines(
        state,
        subtitle_area,
        std::slice::from_mut(&mut subtitle_line),
    );

    frame.render_widget(Paragraph::new(subtitle_line), subtitle_area);

    // Content area below divider
    let content_area = Rect {
        x: panel_area.x,
        y: panel_area.y + 3,
        width: panel_area.width,
        height: panel_area.height - 3,
    };

    render_model_panel(
        state,
        frame,
        content_area,
        ModelPanelParams {
            entries: &entries,
            selected,
            thinking_idx,
            active_provider: &active_provider,
            active_model: &active_model,
        },
    );
}

struct ModelPanelParams<'a> {
    entries: &'a [ModelSelectionEntry],
    selected: usize,
    thinking_idx: usize,
    active_provider: &'a str,
    active_model: &'a str,
}

fn render_model_panel(
    state: &mut UiState,
    frame: &mut ratatui::Frame,
    area: Rect,
    params: ModelPanelParams<'_>,
) {
    let has_thinking = params
        .entries
        .get(params.selected)
        .is_some_and(|e| matches!(e, ModelSelectionEntry::Model { model, .. } if model.thinking));

    // Layout: entries list + [thinking row] + hint
    let hint_h: u16 = 1;
    let thinking_h: u16 = if has_thinking { 1 } else { 0 };
    let gap_h: u16 = if has_thinking { 1 } else { 0 };
    let list_h = area.height.saturating_sub(hint_h + thinking_h + gap_h);

    let list_area = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: list_h,
    };
    let thinking_y = area.y + list_h + gap_h;
    let hint_y = area.y + area.height - 1;

    // Render model entries
    let mut lines: Vec<Line> = Vec::new();
    let mut model_num: usize = 0;

    for (i, entry) in params.entries.iter().enumerate() {
        if lines.len() >= list_h as usize {
            break;
        }
        match entry {
            ModelSelectionEntry::ProviderHeader { name } => {
                lines.push(Line::from(Span::styled(
                    format!("  {}", name),
                    Style::default()
                        .fg(Color::Rgb(140, 145, 155))
                        .add_modifier(Modifier::BOLD),
                )));
            }
            ModelSelectionEntry::Model {
                provider_key,
                model,
            } => {
                model_num += 1;
                let is_sel = i == params.selected;
                let display = model.name.as_deref().unwrap_or(&model.id);

                // Build description from model config
                let mut desc_parts = Vec::new();
                let limit_k = model.limit / 1000;
                if limit_k > 0 {
                    desc_parts.push(format!("{}K context", limit_k));
                }
                if model.thinking {
                    desc_parts.push("thinking".to_string());
                }
                let desc = desc_parts.join(" · ");

                // Checkmark for non-standard providers (custom models)
                let is_active =
                    provider_key == params.active_provider && model.id == params.active_model;
                let checkmark = if is_active { " ✔" } else { "" };

                let number_str = format!("{}.", model_num);

                let selected_color = Color::Rgb(0x42, 0xd9, 0xe8);
                let active_color = Color::Rgb(126, 158, 126);

                if is_sel {
                    let mut spans = vec![
                        Span::styled(" ❯ ", Style::default().fg(selected_color)),
                        Span::styled(
                            format!(" {} {}{}", number_str, display, checkmark),
                            Style::default()
                                .fg(selected_color)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ];
                    if !desc.is_empty() {
                        spans.push(Span::raw("  "));
                        spans.push(Span::styled(desc, Style::default().fg(selected_color)));
                    }
                    lines.push(Line::from(spans));
                } else {
                    let fg_color = if is_active {
                        active_color
                    } else {
                        Color::Rgb(165, 172, 182)
                    };
                    let style = Style::default().fg(fg_color);
                    let name_style = if is_active {
                        Style::default()
                            .fg(active_color)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        style
                    };
                    let mut spans = vec![
                        Span::styled("   ", style),
                        Span::styled(
                            format!(" {} {}{}", number_str, display, checkmark),
                            name_style,
                        ),
                    ];
                    if !desc.is_empty() {
                        spans.push(Span::raw("  "));
                        spans.push(Span::styled(desc, Style::default().fg(fg_color)));
                    }
                    lines.push(Line::from(spans));
                }
            }
        }
    }

    register_and_highlight_lines(state, list_area, &mut lines);
    frame.render_widget(Paragraph::new(Text::from(lines)), list_area);

    // Thinking effort row
    if has_thinking {
        const EFFORT_ICONS: &[&str] = &["○", "◔", "◑", "◉"];
        const EFFORT_LABELS: &[&str] = &["No", "Low", "Medium", "High"];
        const EFFORT_COLORS: &[Color] = &[
            Color::Rgb(140, 145, 155),
            Color::Rgb(190, 170, 140),
            Color::Rgb(220, 185, 145),
            Color::Rgb(255, 200, 120),
        ];
        let ti = params.thinking_idx.min(EFFORT_ICONS.len() - 1);
        let icon = EFFORT_ICONS[ti];
        let label = EFFORT_LABELS[ti];
        let color = EFFORT_COLORS[ti];

        let thinking_style = Style::default().fg(color).add_modifier(if ti > 0 {
            Modifier::BOLD
        } else {
            Modifier::empty()
        });

        let mut thinking_line = Line::from(vec![
            Span::raw("  "),
            Span::styled(format!("{} {} effort", icon, label), thinking_style),
            Span::raw("   "),
            Span::styled(
                "← → to adjust",
                Style::default().fg(Color::Rgb(140, 145, 155)),
            ),
        ]);
        let thinking_area = Rect {
            x: area.x,
            y: thinking_y,
            width: area.width,
            height: 1,
        };
        register_and_highlight_lines(
            state,
            thinking_area,
            std::slice::from_mut(&mut thinking_line),
        );

        frame.render_widget(Paragraph::new(thinking_line), thinking_area);
    }

    // Hint
    let hint_text = if has_thinking {
        "  ↑↓ select  ·  ←→ effort  ·  Enter confirm  ·  Esc cancel"
    } else {
        "  ↑↓ select  ·  Enter confirm  ·  Esc cancel"
    };
    let mut hint = Line::from(Span::styled(
        hint_text,
        Style::default().fg(Color::Rgb(140, 145, 155)),
    ));
    let hint_area = Rect {
        x: area.x,
        y: hint_y,
        width: area.width,
        height: 1,
    };
    register_and_highlight_lines(state, hint_area, std::slice::from_mut(&mut hint));
    frame.render_widget(Paragraph::new(hint), hint_area);
}
