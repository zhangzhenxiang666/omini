use super::*;

pub(super) fn interaction_drawer_height(state: &UiState, area: Rect) -> Option<u16> {
    let Some(InteractionStep::ModelSelection {
        entries, selected, ..
    }) = &state.interaction_step
    else {
        return None;
    };

    let panel_height = model_panel_height(entries, *selected, area.height.saturating_sub(1));
    Some(panel_height.saturating_add(1).min(area.height))
}

pub(super) fn render_interaction(state: &mut UiState, frame: &mut ratatui::Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    if let Some(InteractionStep::Agents(manager)) = &mut state.interaction_step {
        manager.set_draft_wrap_width(super::agents::input_box_inner_width(
            area.width.saturating_sub(4) as usize,
        ));
    }

    if let Some(InteractionStep::Agents(manager)) = state.interaction_step.clone() {
        super::agents::render_agents_panel(state, frame, area, &manager);
        return;
    }

    // ThinkingEffort 现已内联在 ModelSelection 中
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

    let panel_height = model_panel_height(&entries, selected, area.height.saturating_sub(1));

    let panel_area = Rect {
        x: area.x,
        y: area.y + area.height.saturating_sub(panel_height),
        width: area.width,
        height: panel_height,
    };

    // 仅清空——不设背景色
    frame.render_widget(Clear, panel_area);

    // ── 头部：标题 + 副标题 + 粗分隔线 ──
    let accent = Color::Rgb(0x42, 0xd9, 0xe8);

    // 第 0 行：面板上方的粗分隔线（━ 字符，强调色）
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

    // 第 1 行：模型选择标题（强调色加粗）
    if panel_area.height > 1 {
        let mut title_line = Line::from(Span::styled(
            " 选择模型",
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
    }

    // 第 2 行：灰色副标题
    if panel_area.height > 2 {
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
    }

    // 分隔线下方的内容区域
    let content_area = Rect {
        x: panel_area.x,
        y: panel_area.y + 3,
        width: panel_area.width,
        height: panel_area.height.saturating_sub(3),
    };

    if content_area.width > 0 && content_area.height > 0 && content_area.y < panel_area.bottom() {
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
}

fn model_panel_height(entries: &[ModelSelectionEntry], _selected: usize, area_height: u16) -> u16 {
    let has_thinking = model_entries_have_thinking(entries);
    // 标题(1) + 副标题(1) + 分隔线(1) + 条目 + 间距(0-1) + thinking(0-1) + 提示(1)
    let extra: u16 = if has_thinking { 6 } else { 4 };
    ((entries.len() as u16) + extra)
        .clamp(5, 22)
        .min(area_height)
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
    let has_any_thinking = model_entries_have_thinking(params.entries);
    let selected_has_thinking = params
        .entries
        .get(params.selected)
        .is_some_and(|e| matches!(e, ModelSelectionEntry::Model { model, .. } if model.thinking));

    // 布局：条目列表 + [思考行] + 提示
    let hint_h: u16 = 1;
    let thinking_h: u16 = if has_any_thinking { 1 } else { 0 };
    let gap_h: u16 = if has_any_thinking { 1 } else { 0 };
    let list_h = area.height.saturating_sub(hint_h + thinking_h + gap_h);

    let list_area = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: list_h,
    };
    let thinking_y = area.y + list_h + gap_h;
    let hint_y = area.y + area.height - 1;

    // 渲染模型条目
    let mut model_lines: Vec<ScrollableLine> = Vec::new();
    let mut model_num: usize = 0;

    for (i, entry) in params.entries.iter().enumerate() {
        match entry {
            ModelSelectionEntry::ProviderHeader { name } => {
                model_lines.push(ScrollableLine {
                    selected: false,
                    line: Line::from(Span::styled(
                        format!("  {}", name),
                        Style::default()
                            .fg(Color::Rgb(140, 145, 155))
                            .add_modifier(Modifier::BOLD),
                    )),
                });
            }
            ModelSelectionEntry::Model {
                provider_key,
                model,
            } => {
                model_num += 1;
                let is_sel = i == params.selected;
                let display = model.name.as_deref().unwrap_or(&model.id);

                let meta = model_meta_text(model, display);

                // 非标准 provider（自定义模型）打勾标记
                let is_active =
                    provider_key == params.active_provider && model.id == params.active_model;
                let checkmark = if is_active { " ✔" } else { "" };

                let number_str = format!("{}.", model_num);

                let selected_color = Color::Rgb(0x42, 0xd9, 0xe8);
                let active_color = Color::Rgb(126, 158, 126);

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
                    if is_sel {
                        Span::styled(" ❯ ", Style::default().fg(selected_color))
                    } else {
                        Span::styled("   ", style)
                    },
                    Span::styled(
                        format!(" {} {}{}", number_str, display, checkmark),
                        name_style,
                    ),
                ];
                if !meta.is_empty() {
                    spans.push(Span::raw("  "));
                    spans.push(Span::styled(meta, Style::default().fg(fg_color)));
                }
                model_lines.push(ScrollableLine {
                    selected: is_sel,
                    line: Line::from(spans),
                });
            }
        }
    }

    let mut lines = scrollable_lines(model_lines, list_h as usize, "↑ 更多模型", "↓ 更多模型");
    if list_area.width > 0 && list_area.height > 0 {
        register_and_highlight_lines(state, list_area, &mut lines);
        frame.render_widget(Paragraph::new(Text::from(lines)), list_area);
    }

    // 思考力度行
    if has_any_thinking && thinking_y < area.bottom() {
        const EFFORT_ICONS: &[&str] = &["○", "◔", "◑", "◉", "◆", "★"];
        const EFFORT_LABELS: &[&str] = &["No", "Low", "Medium", "High", "XHigh", "Max"];
        const EFFORT_COLORS: &[Color] = &[
            Color::Rgb(140, 145, 155),
            Color::Rgb(190, 170, 140),
            Color::Rgb(220, 185, 145),
            Color::Rgb(255, 200, 120),
            Color::Rgb(255, 170, 120),
            Color::Rgb(255, 135, 135),
        ];
        let ti = params.thinking_idx.min(EFFORT_ICONS.len() - 1);
        let icon = EFFORT_ICONS[ti];
        let label = EFFORT_LABELS[ti];
        let color = EFFORT_COLORS[ti];

        let thinking_style = Style::default()
            .fg(if selected_has_thinking {
                color
            } else {
                Color::Rgb(100, 105, 115)
            })
            .add_modifier(if selected_has_thinking && ti > 0 {
                Modifier::BOLD
            } else {
                Modifier::empty()
            });
        let helper_style = Style::default().fg(Color::Rgb(140, 145, 155));
        let mut thinking_line = if selected_has_thinking {
            Line::from(vec![
                Span::raw("  "),
                Span::styled(format!("{} {} effort", icon, label), thinking_style),
                Span::raw("   "),
                Span::styled("← → to adjust", helper_style),
            ])
        } else {
            Line::from(vec![
                Span::raw("  "),
                Span::styled("○ effort unavailable", thinking_style),
                Span::raw("   "),
                Span::styled("model does not support thinking", helper_style),
            ])
        };
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

    // 提示
    let hint_text = if selected_has_thinking {
        "  ↑↓ 选择  ·  ←→ effort  ·  Enter 确认  ·  Esc 取消"
    } else {
        "  ↑↓ 选择  ·  Enter 确认  ·  Esc 取消"
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
    if hint_area.width > 0 {
        register_and_highlight_lines(state, hint_area, std::slice::from_mut(&mut hint));
        frame.render_widget(Paragraph::new(hint), hint_area);
    }
}

fn model_entries_have_thinking(entries: &[ModelSelectionEntry]) -> bool {
    entries
        .iter()
        .any(|entry| matches!(entry, ModelSelectionEntry::Model { model, .. } if model.thinking))
}

fn model_meta_text(model: &crate::types::config::ModelConfig, display: &str) -> String {
    let mut parts = Vec::new();
    if model.id != display {
        parts.push(model.id.clone());
    }
    if let Some(limit) = format_context_limit(model.limit) {
        parts.push(format!("{limit} 上下文"));
    }
    if model.thinking {
        parts.push("thinking".to_string());
    }
    parts.join(" · ")
}

fn format_context_limit(limit: u32) -> Option<String> {
    if limit >= 1_000_000 {
        let millions = limit as f64 / 1_000_000.0;
        return Some(trim_decimal_unit(millions, "m"));
    }
    if limit >= 1_000 {
        return Some(format!("{}k", limit / 1_000));
    }
    None
}

fn trim_decimal_unit(value: f64, unit: &str) -> String {
    let mut text = format!("{value:.1}");
    if text.ends_with(".0") {
        text.truncate(text.len() - 2);
    }
    format!("{text}{unit}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ModelSelectionEntry;
    use crate::types::config::ModelConfig;

    fn model_entry(id: &str, thinking: bool) -> ModelSelectionEntry {
        ModelSelectionEntry::Model {
            provider_key: "openai".to_string(),
            model: ModelConfig {
                id: id.to_string(),
                name: None,
                limit: 1000,
                thinking,
                input_modalities: None,
                extra_body: None,
                extra_headers: None,
            },
        }
    }

    #[test]
    fn context_limit_formats_million_tokens_as_m() {
        assert_eq!(format_context_limit(1_000_000).as_deref(), Some("1m"));
        assert_eq!(format_context_limit(1_500_000).as_deref(), Some("1.5m"));
        assert_eq!(format_context_limit(256_000).as_deref(), Some("256k"));
    }

    #[test]
    fn model_panel_height_stays_stable_across_thinking_selection() {
        let entries = vec![
            ModelSelectionEntry::ProviderHeader {
                name: "OpenAI".to_string(),
            },
            model_entry("fast", false),
            model_entry("reasoner", true),
        ];

        assert_eq!(
            model_panel_height(&entries, 1, 50),
            model_panel_height(&entries, 2, 50)
        );
    }

    #[test]
    fn model_panel_height_omits_effort_area_without_thinking_models() {
        let entries = vec![
            ModelSelectionEntry::ProviderHeader {
                name: "OpenAI".to_string(),
            },
            model_entry("fast", false),
            model_entry("lite", false),
        ];

        assert_eq!(model_panel_height(&entries, 1, 50), 7);
    }

    #[test]
    fn model_meta_includes_id_when_display_name_differs() {
        let model = ModelConfig {
            id: "gpt-5.4-mini".to_string(),
            name: Some("GPT 5.4 Mini".to_string()),
            limit: 1_000_000,
            thinking: true,
            input_modalities: None,
            extra_body: None,
            extra_headers: None,
        };

        assert_eq!(
            model_meta_text(&model, "GPT 5.4 Mini"),
            "gpt-5.4-mini · 1m 上下文 · thinking"
        );
    }

    #[test]
    fn model_meta_omits_duplicate_id_when_display_is_id() {
        let model = ModelConfig {
            id: "gpt-5.4-mini".to_string(),
            name: None,
            limit: 1_000_000,
            thinking: false,
            input_modalities: None,
            extra_body: None,
            extra_headers: None,
        };

        assert_eq!(model_meta_text(&model, "gpt-5.4-mini"), "1m 上下文");
    }
}
