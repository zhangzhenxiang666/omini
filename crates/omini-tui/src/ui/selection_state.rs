use crate::app::state::*;

#[derive(Debug)]
pub struct SelectionState {
    /// 鼠标拖选状态。
    pub text_selection: Option<TextSelection>,
    pub is_selecting_text: bool,
    /// 自适应滚动步长（根据滚动速度动态调整）
    pub scroll_step: usize,
    /// 上次滚动时间戳（用于速度计算）
    pub last_scroll_time: Option<tokio::time::Instant>,
}

impl Default for SelectionState {
    fn default() -> Self {
        Self {
            text_selection: None,
            is_selecting_text: false,
            scroll_step: 1,
            last_scroll_time: None,
        }
    }
}
