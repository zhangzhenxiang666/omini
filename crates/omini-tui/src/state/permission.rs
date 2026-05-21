use super::UiState;
use crate::types::events::UserInputPreview;
use ratatui::layout::Rect;

impl UiState {
    pub fn reset_permission_drawer(&mut self) {
        self.permission_selected = 0;
        self.user_input_question_index = 0;
        self.user_input_selected.clear();
        self.user_input_answered.clear();
        self.user_input_note_mode = false;
        self.user_input_notes.clear();
        self.user_input_note_cursors.clear();
        self.permission_scroll_offset = usize::MAX;
        self.permission_drawer_area = Rect::default();
        self.permission_drawer_body_area = Rect::default();
        self.permission_drawer_content_len = 0;
    }

    pub fn permission_select_prev(&mut self) {
        if self.user_input_selected.is_empty() {
            self.permission_selected = self.permission_selected.saturating_sub(1);
        } else if let Some(selected) = self
            .user_input_selected
            .get_mut(self.user_input_question_index)
        {
            *selected = selected.saturating_sub(1);
        }
    }

    pub fn permission_select_next_with_max(&mut self, max_selected: usize) {
        if self.user_input_selected.is_empty() {
            self.permission_selected = (self.permission_selected + 1).min(max_selected);
        } else if let Some(selected) = self
            .user_input_selected
            .get_mut(self.user_input_question_index)
        {
            *selected = (*selected + 1).min(max_selected);
        }
    }

    pub fn current_user_input_selected(&self) -> usize {
        self.user_input_selected
            .get(self.user_input_question_index)
            .copied()
            .unwrap_or(self.permission_selected)
    }

    pub fn current_user_input_note(&self) -> &str {
        self.user_input_notes
            .get(self.user_input_question_index)
            .map(String::as_str)
            .unwrap_or("")
    }

    pub fn current_user_input_note_cursor(&self) -> usize {
        self.user_input_note_cursors
            .get(self.user_input_question_index)
            .copied()
            .unwrap_or(0)
    }

    pub fn user_input_unanswered_count(&self) -> usize {
        self.user_input_answered
            .iter()
            .filter(|answered| !**answered)
            .count()
    }

    pub fn user_input_question_next(&mut self) {
        if !self.user_input_selected.is_empty() {
            self.user_input_question_index =
                (self.user_input_question_index + 1).min(self.user_input_selected.len() - 1);
        }
        self.user_input_note_mode = false;
    }

    pub fn user_input_question_prev(&mut self) {
        self.user_input_question_index = self.user_input_question_index.saturating_sub(1);
        self.user_input_note_mode = false;
    }

    pub fn mark_current_user_input_answered(&mut self) {
        if let Some(answered) = self
            .user_input_answered
            .get_mut(self.user_input_question_index)
        {
            *answered = true;
        }
    }

    pub fn move_to_next_unanswered_user_input(&mut self) {
        if let Some((idx, _)) = self
            .user_input_answered
            .iter()
            .enumerate()
            .find(|(_, answered)| !**answered)
        {
            self.user_input_question_index = idx;
            self.user_input_note_mode = false;
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
        let byte_idx = self.note_char_to_byte(self.current_user_input_note_cursor());
        if let Some(note) = self
            .user_input_notes
            .get_mut(self.user_input_question_index)
        {
            note.insert(byte_idx, c);
        }
        if let Some(cursor) = self
            .user_input_note_cursors
            .get_mut(self.user_input_question_index)
        {
            *cursor += 1;
        }
    }

    pub fn delete_note_before(&mut self) {
        let cursor = self.current_user_input_note_cursor();
        if cursor > 0 {
            let new_cursor = cursor - 1;
            let byte_idx = self.note_char_to_byte(new_cursor);
            if let Some(note) = self
                .user_input_notes
                .get_mut(self.user_input_question_index)
            {
                note.remove(byte_idx);
            }
            if let Some(cursor) = self
                .user_input_note_cursors
                .get_mut(self.user_input_question_index)
            {
                *cursor = new_cursor;
            }
        }
    }

    pub fn delete_note_after(&mut self) {
        let cursor = self.current_user_input_note_cursor();
        let byte_idx = self.note_char_to_byte(cursor);
        if let Some(note) = self
            .user_input_notes
            .get_mut(self.user_input_question_index)
            && byte_idx < note.len()
        {
            note.remove(byte_idx);
        }
    }

    pub fn note_cursor_left(&mut self) {
        if let Some(cursor) = self
            .user_input_note_cursors
            .get_mut(self.user_input_question_index)
        {
            *cursor = cursor.saturating_sub(1);
        }
    }

    pub fn note_cursor_right(&mut self) {
        let max_chars = self.current_user_input_note().chars().count();
        if let Some(cursor) = self
            .user_input_note_cursors
            .get_mut(self.user_input_question_index)
            && *cursor < max_chars
        {
            *cursor += 1;
        }
    }

    pub fn note_cursor_home(&mut self) {
        if let Some(cursor) = self
            .user_input_note_cursors
            .get_mut(self.user_input_question_index)
        {
            *cursor = 0;
        }
    }

    pub fn note_cursor_end(&mut self) {
        let len = self.current_user_input_note().chars().count();
        if let Some(cursor) = self
            .user_input_note_cursors
            .get_mut(self.user_input_question_index)
        {
            *cursor = len;
        }
    }

    pub(super) fn prepare_user_input_preview(&mut self, preview: &UserInputPreview) {
        let len = preview.questions.len();
        self.user_input_question_index = 0;
        self.user_input_selected = vec![0; len];
        self.user_input_answered = vec![false; len];
        self.user_input_notes = vec![String::new(); len];
        self.user_input_note_cursors = vec![0; len];
        self.user_input_note_mode = false;
        self.permission_selected = 0;
    }

    pub(super) fn prepare_permission_pause(&mut self) {
        self.user_input_question_index = 0;
        self.user_input_selected.clear();
        self.user_input_answered.clear();
        self.user_input_notes = vec![String::new()];
        self.user_input_note_cursors = vec![0];
        self.user_input_note_mode = false;
        self.permission_selected = 0;
    }

    pub fn permission_scroll_up(&mut self, lines: usize) {
        self.permission_scroll_offset = self.permission_scroll_offset.saturating_add(lines);
    }

    pub fn permission_scroll_down(&mut self, lines: usize) {
        let visible = self.permission_drawer_body_area.height as usize;
        let max_scroll = self.permission_drawer_content_len.saturating_sub(visible);
        let capped_offset = self.permission_scroll_offset.min(max_scroll);
        self.permission_scroll_offset = capped_offset.saturating_sub(lines);
    }
}
