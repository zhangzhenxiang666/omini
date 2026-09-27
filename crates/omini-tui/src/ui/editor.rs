/// 按 Unicode 字符定位的单行/多行备注编辑器。光标不跨越 UTF-8 字节边界。
#[derive(Debug, Default, Clone)]
pub struct NoteEditor {
    pub text: String,
    pub cursor: usize,
    pub editing: bool,
}
impl NoteEditor {
    fn byte(&self, cursor: usize) -> usize {
        self.text.chars().take(cursor).map(char::len_utf8).sum()
    }
    pub fn insert(&mut self, ch: char) {
        self.text.insert(self.byte(self.cursor), ch);
        self.cursor += 1;
    }
    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.text.remove(self.byte(self.cursor));
        }
    }
    pub fn delete(&mut self) {
        let byte = self.byte(self.cursor);
        if byte < self.text.len() {
            self.text.remove(byte);
        }
    }
    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }
    pub fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.text.chars().count());
    }
    pub fn home(&mut self) {
        self.cursor = 0;
    }
    pub fn end(&mut self) {
        self.cursor = self.text.chars().count();
    }
}
