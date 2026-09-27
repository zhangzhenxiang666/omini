//! 将离线图鉴的真实 Ratatui 帧导出为可审阅的 SVG 截图。
use omini_tui::app::debug::{Gallery, SCREEN_NAMES, TOOL_NAMES, render};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, style::Color};
use std::{fmt::Write, fs, path::Path};

// SVG 无法读取观看者的终端主题，预览使用常见深色终端底色模拟 Color::Reset。
const PREVIEW_TERMINAL_BACKGROUND: &str = "#282c34";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/images");
    fs::create_dir_all(&output)?;
    let agent_scene = TOOL_NAMES
        .iter()
        .position(|name| *name == "spawn_agent")
        .expect("样式图鉴包含 Agent 工具");
    for (name, scene, width, height) in [
        ("tui-shell.svg", 0, 120, 36),
        ("tui-agent.svg", agent_scene, 80, 24),
        (
            "tui-input.svg",
            TOOL_NAMES.len() + SCREEN_NAMES.len() - 1,
            120,
            36,
        ),
        ("tui-bash-permission.svg", TOOL_NAMES.len() + 2, 80, 24),
        ("tui-model.svg", TOOL_NAMES.len() + 10, 120, 36),
        ("tui-idle-task.svg", TOOL_NAMES.len() + 21, 80, 24),
    ] {
        let mut terminal = Terminal::new(TestBackend::new(width, height))?;
        terminal.draw(|frame| render(frame, Gallery { scene, scroll: 0 }))?;
        fs::write(output.join(name), to_svg(terminal.backend().buffer()))?;
    }
    Ok(())
}

fn color(color: Color) -> String {
    match color {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Black => "#000000".into(),
        Color::White => "#ffffff".into(),
        _ => "#dedad4".into(),
    }
}

fn escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn to_svg(buffer: &Buffer) -> String {
    let width = buffer.area.width as usize;
    let height = buffer.area.height as usize;
    let (cell_w, cell_h) = (9, 20);
    let mut svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{}\" height=\"{}\" viewBox=\"0 0 {} {}\">\n<rect width=\"100%\" height=\"100%\" fill=\"{PREVIEW_TERMINAL_BACKGROUND}\"/>\n",
        width * cell_w,
        height * cell_h,
        width * cell_w,
        height * cell_h
    );
    for (index, cell) in buffer.content().iter().enumerate() {
        let x = (index % width) * cell_w;
        let y = (index / width) * cell_h;
        if cell.bg != Color::Reset {
            let _ = writeln!(
                svg,
                "<rect x=\"{x}\" y=\"{y}\" width=\"{cell_w}\" height=\"{cell_h}\" fill=\"{}\"/>",
                color(cell.bg)
            );
        }
        let symbol = cell.symbol();
        if symbol.trim().is_empty() {
            continue;
        }
        let _ = writeln!(
            svg,
            "<text x=\"{x}\" y=\"{}\" fill=\"{}\" font-family=\"Menlo, 'Noto Sans Mono CJK SC', monospace\" font-size=\"15\"{}{}>{}</text>",
            y + 15,
            color(cell.fg),
            if cell.modifier.contains(ratatui::style::Modifier::BOLD) {
                " font-weight=\"bold\""
            } else {
                ""
            },
            if cell.modifier.contains(ratatui::style::Modifier::ITALIC) {
                " font-style=\"italic\""
            } else {
                ""
            },
            escape(symbol)
        );
    }
    svg.push_str("</svg>\n");
    svg
}
