use crate::tui::state::UiState;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

pub(super) fn render_autocomplete(state: &UiState, frame: &mut ratatui::Frame, input_area: Rect) {
    if state.mention_autocomplete.visible {
        render_mentions(state, frame, input_area);
        return;
    }

    if !state.autocomplete.visible || state.autocomplete.filtered.is_empty() {
        return;
    }

    let max_items = 6;
    let total = state.autocomplete.filtered.len();
    let selected = state.autocomplete.selected.min(total.saturating_sub(1));
    let start = if selected >= max_items {
        selected + 1 - max_items
    } else {
        0
    };
    let count = total.saturating_sub(start).min(max_items);
    let popup_width = input_area.width;

    let popup_height = count as u16;
    let y = input_area.y.saturating_sub(popup_height);
    let popup_area = Rect {
        x: input_area.x,
        y,
        width: popup_width,
        height: popup_height,
    };

    frame.render_widget(Clear, popup_area);

    let input_bg = Color::Rgb(65, 69, 76);
    frame.render_widget(
        Paragraph::new(Line::from("")).style(Style::default().bg(input_bg)),
        popup_area,
    );

    let border_clr = Color::Rgb(90, 102, 118);
    let sel_bg = Color::Rgb(255, 204, 163);
    let idle_fg = Color::Rgb(165, 172, 182);
    let content_width = popup_width.saturating_sub(4) as usize;

    let cmds: Vec<_> = state.autocomplete.filtered.iter().collect();
    let max_name_width = cmds
        .iter()
        .map(|cmd| format!("/{}", cmd.name).chars().count())
        .max()
        .unwrap_or(0);
    let lines: Vec<Line> = cmds
        .into_iter()
        .skip(start)
        .take(count)
        .enumerate()
        .map(|(i, cmd)| {
            let is_sel = start + i == selected;
            let row_bg = if is_sel { sel_bg } else { input_bg };

            let left = format!("/{}", cmd.name);
            let padding = " ".repeat(max_name_width.saturating_sub(left.chars().count()));
            let text = format!("{}{}  {}", left, padding, cmd.description);
            let text_w = UnicodeWidthStr::width(&text[..]);
            let pad = content_width.saturating_sub(text_w);

            let content_style = Style::default().fg(idle_fg).bg(row_bg);
            let border_style = Style::default().fg(border_clr);

            let mut spans = vec![
                Span::styled("\u{2503}", border_style),
                Span::styled(text, content_style),
            ];
            if pad > 0 {
                spans.push(Span::styled(" ".repeat(pad), content_style));
            }
            spans.push(Span::styled("  ", content_style));
            spans.push(Span::styled("\u{2503}", border_style));

            Line::from(spans)
        })
        .collect();

    frame.render_widget(Paragraph::new(ratatui::text::Text::from(lines)), popup_area);
}

fn render_mentions(state: &UiState, frame: &mut ratatui::Frame, input_area: Rect) {
    let max_items = 8;
    let total = state.mention_autocomplete.filtered.len();
    let selected = state
        .mention_autocomplete
        .selected
        .min(total.saturating_sub(1));
    let start = if selected >= max_items {
        selected + 1 - max_items
    } else {
        0
    };
    let count = total.saturating_sub(start).min(max_items).max(1);
    let popup_width = input_area.width;

    let popup_height = count as u16;
    let y = input_area.y.saturating_sub(popup_height);
    let popup_area = Rect {
        x: input_area.x,
        y,
        width: popup_width,
        height: popup_height,
    };

    frame.render_widget(Clear, popup_area);

    let input_bg = Color::Rgb(65, 69, 76);
    frame.render_widget(
        Paragraph::new(Line::from("")).style(Style::default().bg(input_bg)),
        popup_area,
    );

    let border_clr = Color::Rgb(90, 102, 118);
    let sel_bg = Color::Rgb(255, 204, 163);
    let idle_fg = Color::Rgb(165, 172, 182);
    let kind_fg = Color::Rgb(0x42, 0xd9, 0xe8);
    let content_width = popup_width.saturating_sub(4) as usize;

    if total == 0 {
        let text = "no matches";
        let text_w = UnicodeWidthStr::width(text);
        let pad = content_width.saturating_sub(text_w);
        let content_style = Style::default().fg(idle_fg).bg(input_bg);
        let border_style = Style::default().fg(border_clr);
        let mut spans = vec![
            Span::styled("\u{2503}", border_style),
            Span::styled(text.to_string(), content_style),
        ];
        if pad > 0 {
            spans.push(Span::styled(" ".repeat(pad), content_style));
        }
        spans.push(Span::styled("  ", content_style));
        spans.push(Span::styled("\u{2503}", border_style));
        frame.render_widget(
            Paragraph::new(ratatui::text::Text::from(vec![Line::from(spans)])),
            popup_area,
        );
        return;
    }

    let candidates: Vec<_> = state.mention_autocomplete.filtered.iter().collect();
    let max_name_width = candidates
        .iter()
        .map(|candidate| candidate.drawer_display().chars().count())
        .max()
        .unwrap_or(0);
    let lines: Vec<Line> = candidates
        .into_iter()
        .skip(start)
        .take(count)
        .enumerate()
        .map(|(i, candidate)| {
            let is_sel = start + i == selected;
            let row_bg = if is_sel { sel_bg } else { input_bg };
            let left = candidate.drawer_display();
            let padding = " ".repeat(max_name_width.saturating_sub(left.chars().count()));
            let kind = match candidate.kind {
                crate::types::display::MentionKind::Subagent => "agent",
                crate::types::display::MentionKind::Directory => "dir",
                crate::types::display::MentionKind::File => "file",
                crate::types::display::MentionKind::Command => "cmd",
            };
            let text = format!(
                "{}{}  {:<5}  {}",
                left, padding, kind, candidate.description
            );
            let text_w = UnicodeWidthStr::width(&text[..]);
            let pad = content_width.saturating_sub(text_w);

            let content_style = Style::default().fg(idle_fg).bg(row_bg);
            let kind_style = Style::default().fg(kind_fg).bg(row_bg);
            let border_style = Style::default().fg(border_clr);
            let mut spans = vec![
                Span::styled("\u{2503}", border_style),
                Span::styled(format!("{}{}  ", left, padding), content_style),
                Span::styled(format!("{kind:<5}"), kind_style),
                Span::styled(format!("  {}", candidate.description), content_style),
            ];
            if pad > 0 {
                spans.push(Span::styled(" ".repeat(pad), content_style));
            }
            spans.push(Span::styled("  ", content_style));
            spans.push(Span::styled("\u{2503}", border_style));
            Line::from(spans)
        })
        .collect();

    frame.render_widget(Paragraph::new(ratatui::text::Text::from(lines)), popup_area);
}
