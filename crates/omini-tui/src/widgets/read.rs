use crate::types::events::{PermissionPreview, ToolPauseKind, ToolPauseRequest};
use crate::types::message::{ToolResultBlock, ToolUseBlock};
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
    let mut lines: Vec<Line> = Vec::new();

    let file_path = tool_use
        .input
        .get("file_path")
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

    main_spans.push(Span::raw("· "));
    if is_permission_preview {
        main_spans.push(Span::styled(display_file_path, title_style));
    } else {
        main_spans.push(Span::styled("Read", title_style));
        main_spans.push(Span::raw(format!(" {}", display_file_path)));
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
