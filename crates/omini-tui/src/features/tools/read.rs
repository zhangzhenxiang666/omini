use crate::app::event::{PermissionPreview, ToolPauseKind, ToolPauseRequest};
use omini_model::message::{ToolResultBlock, ToolUseBlock};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use std::path::Path;

use super::{display_path, tool_error_display_text, tool_title_style, word_wrap};

pub fn render(
    tool_use: &ToolUseBlock,
    result: Option<&ToolResultBlock>,
    preview: Option<&ToolPauseRequest>,
    content_width: usize,
    project_dir: Option<&Path>,
) -> Vec<Line<'static>> {
    render_path_tool(
        tool_use,
        result,
        preview,
        content_width,
        project_dir,
        "Read",
        "file_path",
    )
}

pub fn render_view_image(
    tool_use: &ToolUseBlock,
    result: Option<&ToolResultBlock>,
    preview: Option<&ToolPauseRequest>,
    content_width: usize,
    project_dir: Option<&Path>,
) -> Vec<Line<'static>> {
    render_path_tool(
        tool_use,
        result,
        preview,
        content_width,
        project_dir,
        "Image",
        "path",
    )
}

fn render_path_tool(
    tool_use: &ToolUseBlock,
    result: Option<&ToolResultBlock>,
    preview: Option<&ToolPauseRequest>,
    content_width: usize,
    project_dir: Option<&Path>,
    title: &'static str,
    path_key: &'static str,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line> = Vec::new();

    let file_path = tool_use
        .input
        .get(path_key)
        .and_then(|v| v.as_str())
        .unwrap_or("<unknown>");
    let display_file_path = display_path(file_path, project_dir);

    let read_color = crate::ui::theme::ACCENT;
    let mut main_spans = Vec::new();
    let is_permission_preview = result.is_none()
        && matches!(
            preview.map(|req| &req.kind),
            Some(ToolPauseKind::Permission(PermissionPreview::Read(_)))
        );
    let title_style = tool_title_style(read_color);

    let params_desc = {
        let limit = tool_use.input.get("limit").and_then(|v| v.as_u64());
        let offset = tool_use.input.get("offset").and_then(|v| v.as_u64());
        let mut s = String::new();
        if let Some(o) = offset {
            s.push_str(&format!(" [offset\u{003d}{o}]"));
        }
        if let Some(l) = limit {
            if s.is_empty() {
                s.push_str(&format!(" [limit\u{003d}{l}]"));
            } else {
                let trimmed = s.trim_end_matches(']');
                s = format!("{trimmed}, limit\u{003d}{l}]");
            }
        }
        s
    };

    main_spans.push(Span::styled(
        if title == "Image" { "◇ " } else { "▤ " },
        Style::default().fg(crate::ui::theme::MUTED),
    ));
    if is_permission_preview {
        let display = format!("{display_file_path}{params_desc}");
        main_spans.push(Span::styled(display, title_style));
    } else {
        main_spans.push(Span::styled(title, title_style));
        main_spans.push(Span::raw(format!(" {display_file_path}{params_desc}")));
    }

    lines.push(Line::from(main_spans));

    if title == "Image"
        && let Some(metadata) = result.and_then(|result| result.metadata.as_ref())
    {
        let detail = metadata
            .iter()
            .filter(|(key, _)| !matches!(key.as_str(), "data" | "base64"))
            .map(|(key, value)| format!("{key}: {value}"))
            .collect::<Vec<_>>()
            .join(" · ");
        for line in word_wrap(&detail, content_width.saturating_sub(4).max(1))
            .into_iter()
            .take(3)
        {
            lines.push(Line::from(Span::styled(
                format!("    {line}"),
                Style::default().fg(crate::ui::theme::MUTED),
            )));
        }
    }
    if let Some(tr) = result
        && tr.is_error
    {
        let error_style = Style::default().fg(crate::ui::theme::ERROR);
        let display = tool_error_display_text(&tr.content);
        let wrapped = word_wrap(&display, content_width.saturating_sub(2));
        for wl in wrapped {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(wl, error_style),
            ]));
        }
    }

    lines
}
