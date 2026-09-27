#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct SelectionPoint {
    /// 可选中渲染行的绝对终端行号。
    pub row: usize,
    /// 可选中渲染行内的显示列号。
    pub col: usize,
}

#[derive(Debug, Clone, Default)]
pub struct TextSelection {
    pub start: SelectionPoint,
    pub end: SelectionPoint,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectableScreenLine {
    pub row: u16,
    pub col: u16,
    pub width: u16,
    pub text: String,
}
