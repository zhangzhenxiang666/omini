use crate::types::events::{PermissionPreview, ToolPauseKind, ToolPauseRequest};
use omini_domain::message::{ToolResultBlock, ToolUseBlock};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use std::path::Path;

use super::{display_path, tool_error_display_text, tool_title_style, word_wrap};

pub(super) fn render(
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

pub(super) fn render_view_image(
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
        "View Image",
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

    let read_color = Color::Rgb(0x42, 0xb3, 0xc2);
    let mut main_spans = Vec::new();
    let is_permission_preview = result.is_none()
        && matches!(
            preview.map(|req| &req.kind),
            Some(ToolPauseKind::Permission(PermissionPreview::Read(_)))
        );
    let is_pending = result.is_none() && !is_permission_preview;
    let title_style = tool_title_style(read_color, is_pending);

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

    main_spans.push(Span::raw("· "));
    if is_permission_preview {
        let display = format!("{display_file_path}{params_desc}");
        main_spans.push(Span::styled(display, title_style));
    } else {
        main_spans.push(Span::styled(title, title_style));
        main_spans.push(Span::raw(format!(" {display_file_path}{params_desc}")));
    }

    lines.push(Line::from(main_spans));

    if let Some(tr) = result
        && tr.is_error
    {
        let error_style = Style::default().fg(Color::Rgb(255, 100, 100));
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
