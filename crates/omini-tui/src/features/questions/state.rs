#[derive(Debug, Default)]
pub struct AskState {
    /// 用户问题抽屉当前题目索引。
    pub user_input_question_index: usize,
    /// 用户问题抽屉每题当前选项索引。
    pub user_input_selected: Vec<usize>,
    /// 用户问题抽屉每题是否已确认答案。
    pub user_input_answered: Vec<bool>,
    /// 用户问题抽屉是否正在编辑当前问题 note。
    pub user_input_note_mode: bool,
    /// 用户问题抽屉每题 note。
    pub user_input_notes: Vec<String>,
    /// 用户问题抽屉每题 note 光标偏移量，按 Unicode 字符计数。
    pub user_input_note_cursors: Vec<usize>,
}
