use crate::app::event::{ToolPauseKind, UserInputPreview};
use crate::app::state::AppState;
use ratatui::layout::Rect;

impl AppState {
    pub fn permission_active(&self) -> bool {
        matches!(
            self.active_tool_pause().map(|request| &request.kind),
            Some(ToolPauseKind::Permission(_))
        )
    }
    pub fn note_mode(&self) -> bool {
        if self.permission_active() {
            self.dialogs.permission.note.editing
        } else {
            self.dialogs.ask.user_input_note_mode
        }
    }
    pub fn set_note_mode(&mut self, editing: bool) {
        if self.permission_active() {
            self.dialogs.permission.note.editing = editing;
        } else {
            self.dialogs.ask.user_input_note_mode = editing;
        }
    }

    pub fn prepare_active_tool_pause(&mut self) {
        let Some(req) = self.active_tool_pause().cloned() else {
            self.reset_permission_drawer();
            return;
        };

        self.reset_permission_drawer();
        match &req.kind {
            ToolPauseKind::Permission(_) => self.prepare_permission_pause(),
            ToolPauseKind::UserInput(preview) => self.prepare_user_input_preview(preview),
        }
    }

    pub fn reset_permission_drawer(&mut self) {
        self.dialogs.permission.note = Default::default();
        self.dialogs.permission.permission_selected = 0;
        self.dialogs.ask.user_input_question_index = 0;
        self.dialogs.ask.user_input_selected.clear();
        self.dialogs.ask.user_input_answered.clear();
        self.dialogs.ask.user_input_note_mode = false;
        self.dialogs.ask.user_input_notes.clear();
        self.dialogs.ask.user_input_note_cursors.clear();
        self.dialogs.permission.permission_scroll_offset = usize::MAX;
        self.geometry.permission_drawer_area = Rect::default();
        self.geometry.permission_drawer_body_area = Rect::default();
        self.geometry.permission_drawer_content_len = 0;
    }

    pub fn permission_select_prev(&mut self) {
        if self.dialogs.ask.user_input_selected.is_empty() {
            self.dialogs.permission.permission_selected = self
                .dialogs
                .permission
                .permission_selected
                .saturating_sub(1);
        } else if let Some(selected) = self
            .dialogs
            .ask
            .user_input_selected
            .get_mut(self.dialogs.ask.user_input_question_index)
        {
            *selected = selected.saturating_sub(1);
        }
    }

    pub fn permission_select_next_with_max(&mut self, max_selected: usize) {
        if self.dialogs.ask.user_input_selected.is_empty() {
            self.dialogs.permission.permission_selected =
                (self.dialogs.permission.permission_selected + 1).min(max_selected);
        } else if let Some(selected) = self
            .dialogs
            .ask
            .user_input_selected
            .get_mut(self.dialogs.ask.user_input_question_index)
        {
            *selected = (*selected + 1).min(max_selected);
        }
    }

    pub fn current_user_input_selected(&self) -> usize {
        self.dialogs
            .ask
            .user_input_selected
            .get(self.dialogs.ask.user_input_question_index)
            .copied()
            .unwrap_or(self.dialogs.permission.permission_selected)
    }

    pub fn current_user_input_note(&self) -> &str {
        if self.permission_active() {
            return &self.dialogs.permission.note.text;
        }
        self.dialogs
            .ask
            .user_input_notes
            .get(self.dialogs.ask.user_input_question_index)
            .map(String::as_str)
            .unwrap_or("")
    }

    pub fn current_user_input_note_cursor(&self) -> usize {
        if self.permission_active() {
            return self.dialogs.permission.note.cursor;
        }
        self.dialogs
            .ask
            .user_input_note_cursors
            .get(self.dialogs.ask.user_input_question_index)
            .copied()
            .unwrap_or(0)
    }

    pub fn user_input_unanswered_count(&self) -> usize {
        self.dialogs
            .ask
            .user_input_answered
            .iter()
            .filter(|answered| !**answered)
            .count()
    }

    pub fn user_input_question_next(&mut self) {
        if !self.dialogs.ask.user_input_selected.is_empty() {
            self.dialogs.ask.user_input_question_index =
                (self.dialogs.ask.user_input_question_index + 1)
                    .min(self.dialogs.ask.user_input_selected.len() - 1);
        }
        self.dialogs.ask.user_input_note_mode = false;
    }

    pub fn user_input_question_prev(&mut self) {
        self.dialogs.ask.user_input_question_index =
            self.dialogs.ask.user_input_question_index.saturating_sub(1);
        self.dialogs.ask.user_input_note_mode = false;
    }

    pub fn mark_current_user_input_answered(&mut self) {
        if let Some(answered) = self
            .dialogs
            .ask
            .user_input_answered
            .get_mut(self.dialogs.ask.user_input_question_index)
        {
            *answered = true;
        }
    }

    pub fn move_to_next_unanswered_user_input(&mut self) {
        if let Some((idx, _)) = self
            .dialogs
            .ask
            .user_input_answered
            .iter()
            .enumerate()
            .find(|(_, answered)| !**answered)
        {
            self.dialogs.ask.user_input_question_index = idx;
            self.dialogs.ask.user_input_note_mode = false;
        }
    }

    fn note_char_to_byte(&self, char_idx: usize) -> usize {
        self.current_user_input_note()
            .chars()
            .take(char_idx)
            .map(char::len_utf8)
            .sum()
    }

    pub fn insert_note_char(&mut self, c: char) {
        if self.permission_active() {
            self.dialogs.permission.note.insert(c);
            return;
        }
        let byte_idx = self.note_char_to_byte(self.current_user_input_note_cursor());
        if let Some(note) = self
            .dialogs
            .ask
            .user_input_notes
            .get_mut(self.dialogs.ask.user_input_question_index)
        {
            note.insert(byte_idx, c);
        }
        if let Some(cursor) = self
            .dialogs
            .ask
            .user_input_note_cursors
            .get_mut(self.dialogs.ask.user_input_question_index)
        {
            *cursor += 1;
        }
    }

    pub fn delete_note_before(&mut self) {
        if self.permission_active() {
            self.dialogs.permission.note.backspace();
            return;
        }
        let cursor = self.current_user_input_note_cursor();
        if cursor > 0 {
            let new_cursor = cursor - 1;
            let byte_idx = self.note_char_to_byte(new_cursor);
            if let Some(note) = self
                .dialogs
                .ask
                .user_input_notes
                .get_mut(self.dialogs.ask.user_input_question_index)
            {
                note.remove(byte_idx);
            }
            if let Some(cursor) = self
                .dialogs
                .ask
                .user_input_note_cursors
                .get_mut(self.dialogs.ask.user_input_question_index)
            {
                *cursor = new_cursor;
            }
        }
    }

    pub fn delete_note_after(&mut self) {
        if self.permission_active() {
            self.dialogs.permission.note.delete();
            return;
        }
        let cursor = self.current_user_input_note_cursor();
        let byte_idx = self.note_char_to_byte(cursor);
        if let Some(note) = self
            .dialogs
            .ask
            .user_input_notes
            .get_mut(self.dialogs.ask.user_input_question_index)
            && byte_idx < note.len()
        {
            note.remove(byte_idx);
        }
    }

    pub fn note_cursor_left(&mut self) {
        if self.permission_active() {
            self.dialogs.permission.note.left();
            return;
        }
        if let Some(cursor) = self
            .dialogs
            .ask
            .user_input_note_cursors
            .get_mut(self.dialogs.ask.user_input_question_index)
        {
            *cursor = cursor.saturating_sub(1);
        }
    }

    pub fn note_cursor_right(&mut self) {
        if self.permission_active() {
            self.dialogs.permission.note.right();
            return;
        }
        let max_chars = self.current_user_input_note().chars().count();
        if let Some(cursor) = self
            .dialogs
            .ask
            .user_input_note_cursors
            .get_mut(self.dialogs.ask.user_input_question_index)
            && *cursor < max_chars
        {
            *cursor += 1;
        }
    }

    pub fn note_cursor_home(&mut self) {
        if self.permission_active() {
            self.dialogs.permission.note.home();
            return;
        }
        if let Some(cursor) = self
            .dialogs
            .ask
            .user_input_note_cursors
            .get_mut(self.dialogs.ask.user_input_question_index)
        {
            *cursor = 0;
        }
    }

    pub fn note_cursor_end(&mut self) {
        if self.permission_active() {
            self.dialogs.permission.note.end();
            return;
        }
        let len = self.current_user_input_note().chars().count();
        if let Some(cursor) = self
            .dialogs
            .ask
            .user_input_note_cursors
            .get_mut(self.dialogs.ask.user_input_question_index)
        {
            *cursor = len;
        }
    }

    pub fn prepare_user_input_preview(&mut self, preview: &UserInputPreview) {
        let len = preview.questions.len();
        self.dialogs.ask.user_input_question_index = 0;
        self.dialogs.ask.user_input_selected = vec![0; len];
        self.dialogs.ask.user_input_answered = vec![false; len];
        self.dialogs.ask.user_input_notes = vec![String::new(); len];
        self.dialogs.ask.user_input_note_cursors = vec![0; len];
        self.dialogs.ask.user_input_note_mode = false;
        self.dialogs.permission.permission_selected = 0;
    }

    pub fn prepare_permission_pause(&mut self) {
        self.dialogs.ask.user_input_question_index = 0;
        self.dialogs.ask.user_input_selected.clear();
        self.dialogs.ask.user_input_answered.clear();
        self.dialogs.ask.user_input_notes.clear();
        self.dialogs.ask.user_input_note_cursors.clear();
        self.dialogs.ask.user_input_note_mode = false;
        self.dialogs.permission.permission_selected = 0;
    }

    pub fn permission_scroll_up(&mut self, lines: usize) {
        self.dialogs.permission.permission_scroll_offset = self
            .dialogs
            .permission
            .permission_scroll_offset
            .saturating_add(lines);
    }

    pub fn permission_scroll_down(&mut self, lines: usize) {
        let visible = self.geometry.permission_drawer_body_area.height as usize;
        let max_scroll = self
            .geometry
            .permission_drawer_content_len
            .saturating_sub(visible);
        let capped_offset = self
            .dialogs
            .permission
            .permission_scroll_offset
            .min(max_scroll);
        self.dialogs.permission.permission_scroll_offset = capped_offset.saturating_sub(lines);
    }
}
