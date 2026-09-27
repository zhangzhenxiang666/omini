use crate::app::state::*;
use ratatui::layout::Rect;

#[derive(Debug, Clone, Default)]
pub struct FrameGeometry {
    /// 消息区域的位置和大小
    pub messages_area: Rect,
    /// 当前屏幕上可由鼠标拖选复制的文本行。
    pub selectable_screen_lines: Vec<SelectableScreenLine>,
    /// 当前权限抽屉整体区域，用于鼠标事件命中判断。
    pub permission_drawer_area: Rect,
    /// 当前权限抽屉可滚动内容区域，用于鼠标滚动命中判断。
    pub permission_drawer_body_area: Rect,
    /// 当前权限抽屉内容总行数。
    pub permission_drawer_content_len: usize,
}
