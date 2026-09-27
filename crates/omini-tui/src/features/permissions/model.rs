#[derive(Debug, Default)]
pub struct PermissionState {
    pub note: crate::ui::editor::NoteEditor,
    /// 权限抽屉当前选中的操作：0 = Yes, 1 = No。
    pub permission_selected: usize,
    /// 权限抽屉从底部向上滚动的行数（0 = 位于底部）。
    pub permission_scroll_offset: usize,
}
