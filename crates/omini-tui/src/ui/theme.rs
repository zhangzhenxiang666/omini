//! 终端默认底色与暖琥珀主题。颜色按语义命名，所有功能视图共享同一套色板。
use ratatui::style::Color;
use ratatui::{
    layout::Rect,
    style::Style,
    widgets::{Block, Clear},
};

pub const BACKGROUND: Color = Color::Reset;
pub const ON_ACCENT: Color = Color::Rgb(0x22, 0x25, 0x2A);
pub const PANEL: Color = Color::Rgb(0x28, 0x2C, 0x32);
pub const INPUT_BG: Color = PANEL;
pub const USER_MESSAGE_BG: Color = Color::Rgb(58, 58, 58);
pub const TEXT: Color = Color::Rgb(0xDE, 0xDA, 0xD4);
pub const MUTED: Color = Color::Rgb(0x96, 0x95, 0x8F);
pub const BORDER: Color = Color::Rgb(0x62, 0x67, 0x70);
pub const ACCENT: Color = Color::Rgb(0xD6, 0xAE, 0x73);
pub const SELECTED: Color = Color::Rgb(0x39, 0x34, 0x2C);
pub const RUNNING: Color = Color::Rgb(0x91, 0xA7, 0xB8);
pub const SUCCESS: Color = Color::Rgb(0x92, 0xB5, 0x8B);
pub const WAITING: Color = ACCENT;
pub const ERROR: Color = Color::Rgb(0xCF, 0x83, 0x7F);
pub const SECONDARY: Color = Color::Rgb(0xB1, 0xA1, 0xC4);
pub const ADD_BG: Color = Color::Rgb(0x2A, 0x36, 0x2C);
pub const DELETE_BG: Color = Color::Rgb(0x3A, 0x2B, 0x2B);
pub const ANIMATION_LOW: Color = Color::Rgb(0x73, 0x67, 0x55);

/// 清除被面板覆盖的旧文字后，重新给每个终端单元格着色。
pub fn clear_panel(frame: &mut ratatui::Frame, area: Rect) {
    frame.render_widget(Clear, area);
    frame.render_widget(
        Block::default().style(Style::default().fg(TEXT).bg(PANEL)),
        area,
    );
}
