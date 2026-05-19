use crate::types::events::{PermissionPreview, ToolPauseKind, ToolPauseRequest};
use crate::types::message::{ToolResultBlock, ToolUseBlock};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use std::path::Path;

use super::{display_path, spinner, word_wrap};

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

    if result.is_none() {
        let spin = spinner();
        main_spans.push(Span::styled(
            format!("{} ", spin),
            Style::default().fg(Color::Rgb(212, 182, 106)),
        ));
    }

    main_spans.push(Span::raw("· "));
    if is_permission_preview {
        main_spans.push(Span::styled(
            display_file_path,
            Style::default().fg(read_color),
        ));
    } else {
        main_spans.push(Span::styled("Read", Style::default().fg(read_color)));
        main_spans.push(Span::raw(format!(" {}", display_file_path)));
    }

    lines.push(Line::from(main_spans));

    if let Some(tr) = result
        && tr.is_error
    {
        let error_style = Style::default().fg(Color::Rgb(255, 100, 100));
        let wrapped = word_wrap(&tr.content, content_width.saturating_sub(2));
        for wl in wrapped {
            lines.push(Line::from(vec![Span::styled(wl, error_style)]));
        }
    }

    lines
}
