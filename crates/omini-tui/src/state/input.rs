use super::{
    InputImageAttachment, InputMention, InputPasteMarker, InputVisualLine, MAX_INPUT_VISIBLE_LINES,
    MentionCandidate, PASTE_MARKER_THRESHOLD_CHARS, PASTE_MARKER_THRESHOLD_NEWLINES, UiState,
};
use omini_domain::display::{DisplayImageAttachment, DisplayMention, MentionKind, UserDraft};
use std::path::{Path, PathBuf};
use unicode_width::UnicodeWidthChar;

const INPUT_PROMPT_WIDTH: usize = 2;

impl UiState {
    pub fn char_to_byte(&self, char_idx: usize) -> usize {
        char_to_byte(&self.input, char_idx)
    }

    pub fn set_input_wrap_width(&mut self, width: usize) {
        let width = width.max(1);
        if self.input_wrap_width != width {
            self.input_wrap_width = width;
            self.ensure_input_cursor_visible();
        }
    }

    pub fn insert_char(&mut self, c: char) {
        self.insert_text(&c.to_string());
        if matches!(c, '\'' | '"') {
            self.replace_quoted_absolute_image_path_before_cursor(c);
        }
    }

    pub fn insert_paste(&mut self, text: String) {
        if let Some(path) = self.existing_image_path_from_pasted_text(&text) {
            self.insert_image_attachment(path);
            self.ensure_input_cursor_visible();
            return;
        }

        let full_char_count = text.chars().count();
        let newline_count = text.chars().filter(|ch| *ch == '\n').count();
        if full_char_count > PASTE_MARKER_THRESHOLD_CHARS
            || newline_count >= PASTE_MARKER_THRESHOLD_NEWLINES
        {
            let marker = format!("[Pasted Content {full_char_count} chars]");
            let start = self.cursor_char;
            self.insert_text(&marker);
            let marker_len = marker.chars().count();
            self.input_paste_markers.push(InputPasteMarker {
                start_char: start,
                end_char: start + marker_len,
                marker,
                full_text: text,
                full_char_count,
            });
            self.input_paste_markers.sort_by_key(|item| item.start_char);
        } else {
            self.insert_text(&text);
        }
        self.ensure_input_cursor_visible();
    }

    pub fn insert_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }

        let byte_idx = self.char_to_byte(self.cursor_char);
        self.input.insert_str(byte_idx, text);
        let inserted_len = text.chars().count();
        self.apply_input_edit(self.cursor_char, 0, inserted_len);
        self.cursor_char += inserted_len;
        self.ensure_input_cursor_visible();
    }

    pub(super) fn insert_image_attachment(&mut self, path: PathBuf) {
        let start = self.cursor_char;
        let marker = self.next_image_marker();
        let replacement = format!("{marker} ");
        self.insert_text(&replacement);
        self.input_images.push(InputImageAttachment {
            start_char: start,
            end_char: start + replacement.chars().count(),
            marker,
            source_path: path.to_string_lossy().to_string(),
            file_name: path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string(),
        });
        self.input_images.sort_by_key(|item| item.start_char);
    }

    fn replace_range_with_image_attachment(&mut self, start: usize, end: usize, path: PathBuf) {
        let marker = self.next_image_marker();
        let replacement = format!("{marker} ");
        let start_byte = char_to_byte(&self.input, start);
        let end_byte = char_to_byte(&self.input, end);
        self.input.replace_range(start_byte..end_byte, &replacement);

        let old_len = end.saturating_sub(start);
        let new_len = replacement.chars().count();
        self.apply_input_edit(start, old_len, new_len);
        self.input_images.push(InputImageAttachment {
            start_char: start,
            end_char: start + new_len,
            marker,
            source_path: path.to_string_lossy().to_string(),
            file_name: path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string(),
        });
        self.input_images.sort_by_key(|item| item.start_char);
        self.cursor_char = start + new_len;
    }

    fn replace_quoted_absolute_image_path_before_cursor(&mut self, quote: char) -> bool {
        if self.cursor_char < 2 {
            return false;
        }

        let chars = self.input.chars().collect::<Vec<_>>();
        let closing_idx = self.cursor_char.saturating_sub(1);
        if chars.get(closing_idx).copied() != Some(quote) {
            return false;
        }

        let Some(opening_idx) = chars[..closing_idx].iter().rposition(|ch| *ch == quote) else {
            return false;
        };
        if opening_idx + 1 == closing_idx {
            return false;
        }

        let path_text = chars[opening_idx + 1..closing_idx]
            .iter()
            .collect::<String>();
        if path_text.contains('\n') || path_text.contains('\r') {
            return false;
        }

        let path = PathBuf::from(&path_text);
        if !path.is_absolute() {
            return false;
        }

        let Some(path) = existing_image_path(path) else {
            return false;
        };

        self.replace_range_with_image_attachment(opening_idx, closing_idx + 1, path);
        true
    }

    fn next_image_marker(&self) -> String {
        format!("[Image#{}]", self.input_images.len() + 1)
    }

    fn existing_image_path_for_target(&self, target: &str) -> Option<PathBuf> {
        let path = PathBuf::from(target);
        let path = if path.is_absolute() {
            path
        } else {
            self.status_bar.cwd.join(path)
        };
        existing_image_path(path)
    }

    fn existing_image_path_from_pasted_text(&self, text: &str) -> Option<PathBuf> {
        let path_text = unquote_single_pasted_path(text)?;
        let path = PathBuf::from(path_text);
        let path = if path.is_absolute() {
            path
        } else {
            self.status_bar.cwd.join(path)
        };
        existing_image_path(path)
    }

    pub fn delete_before(&mut self) {
        if self.cursor_char > 0 {
            if let Some((start, end)) = self.input_atom_before_cursor() {
                self.delete_input_range(start, end);
                self.cursor_char = start;
                self.ensure_input_cursor_visible();
                return;
            }

            self.cursor_char -= 1;
            let byte_idx = self.char_to_byte(self.cursor_char);
            self.input.remove(byte_idx);
            self.apply_input_edit(self.cursor_char, 1, 0);
            self.ensure_input_cursor_visible();
        }
    }

    pub fn delete_after(&mut self) {
        if let Some((start, end)) = self.input_atom_after_cursor() {
            self.delete_input_range(start, end);
            self.cursor_char = start;
            self.ensure_input_cursor_visible();
            return;
        }

        let byte_idx = self.char_to_byte(self.cursor_char);
        if byte_idx < self.input.len() {
            self.input.remove(byte_idx);
            self.apply_input_edit(self.cursor_char, 1, 0);
            self.ensure_input_cursor_visible();
        }
    }

    pub fn cursor_left(&mut self) {
        if let Some((start, _)) = self.input_atom_before_cursor() {
            self.cursor_char = start;
            self.ensure_input_cursor_visible();
            return;
        }

        self.cursor_char = self.cursor_char.saturating_sub(1);
        self.ensure_input_cursor_visible();
    }

    pub fn cursor_right(&mut self) {
        if let Some((_, end)) = self.input_atom_after_cursor() {
            self.cursor_char = end;
            self.ensure_input_cursor_visible();
            return;
        }

        let max_chars = self.input.chars().count();
        if self.cursor_char < max_chars {
            self.cursor_char += 1;
        }
        self.ensure_input_cursor_visible();
    }

    pub fn cursor_home(&mut self) {
        self.cursor_char = 0;
        self.ensure_input_cursor_visible();
    }

    pub fn cursor_end(&mut self) {
        self.cursor_char = self.input.chars().count();
        self.ensure_input_cursor_visible();
    }

    pub fn cursor_up_in_input(&mut self) -> bool {
        let Some((line_idx, col)) = self.input_cursor_line_col() else {
            return false;
        };
        if line_idx == 0 {
            return false;
        }
        self.cursor_char = self.input_line_col_to_char(line_idx - 1, col);
        self.ensure_input_cursor_visible();
        true
    }

    pub fn cursor_down_in_input(&mut self) -> bool {
        let Some((line_idx, col)) = self.input_cursor_line_col() else {
            return false;
        };
        let line_count = self.input_line_count();
        if line_idx + 1 >= line_count {
            return false;
        }
        self.cursor_char = self.input_line_col_to_char(line_idx + 1, col);
        self.ensure_input_cursor_visible();
        true
    }

    pub fn input_line_count(&self) -> usize {
        self.input_visual_lines().len()
    }

    pub fn input_visible_line_count(&self) -> usize {
        self.input_line_count().clamp(1, MAX_INPUT_VISIBLE_LINES)
    }

    pub fn ensure_input_cursor_visible(&mut self) {
        let line_idx = self
            .input_cursor_line_col()
            .map(|(line_idx, _)| line_idx)
            .unwrap_or(0);
        let visible = self.input_visible_line_count();
        if line_idx < self.input_scroll_line {
            self.input_scroll_line = line_idx;
        } else if line_idx >= self.input_scroll_line + visible {
            self.input_scroll_line = line_idx + 1 - visible;
        }
        let max_scroll = self.input_line_count().saturating_sub(visible);
        self.input_scroll_line = self.input_scroll_line.min(max_scroll);
    }

    pub fn update_input_autocomplete(&mut self) {
        self.autocomplete.update(&self.input);
        if self.autocomplete.visible {
            if self.mention_autocomplete.visible {
                self.mention_autocomplete.clear_session_cache();
            }
            self.mention_autocomplete.visible = false;
        } else {
            self.mention_autocomplete
                .update(&self.input, self.cursor_char);
        }
    }

    pub fn set_mention_context(&mut self, cwd: PathBuf, candidates: Vec<MentionCandidate>) {
        self.mention_autocomplete.set_cwd(cwd);
        self.mention_autocomplete.set_candidates(candidates);
        self.update_input_autocomplete();
    }

    pub fn insert_selected_mention(&mut self) -> bool {
        let Some(candidate) = self.mention_autocomplete.selected_candidate().cloned() else {
            return false;
        };
        let start = self.mention_autocomplete.active_start;
        let end = self.mention_autocomplete.active_end;

        if candidate.kind == MentionKind::File
            && let Some(path) = self.existing_image_path_for_target(&candidate.target)
        {
            self.replace_range_with_image_attachment(start, end, path);
            self.mention_autocomplete.visible = false;
            self.mention_autocomplete.clear_session_cache();
            self.autocomplete.visible = false;
            self.ensure_input_cursor_visible();
            return true;
        }

        let display = candidate.insert_display();
        let replacement = format!("{display} ");
        let start_byte = char_to_byte(&self.input, start);
        let end_byte = char_to_byte(&self.input, end);
        self.input.replace_range(start_byte..end_byte, &replacement);

        let old_len = end.saturating_sub(start);
        let new_len = replacement.chars().count();
        self.apply_input_edit(start, old_len, new_len);
        let mention_len = replacement.chars().count();
        self.input_mentions.push(InputMention {
            start_char: start,
            end_char: start + mention_len,
            kind: candidate.kind,
            label: candidate.label,
            target: candidate.target,
            description: candidate.description,
        });
        self.input_mentions.sort_by_key(|item| item.start_char);
        self.cursor_char = start + new_len;
        self.ensure_input_cursor_visible();
        self.mention_autocomplete.visible = false;
        self.mention_autocomplete.clear_session_cache();
        self.autocomplete.visible = false;
        true
    }

    pub fn expand_selected_mention_directory(&mut self) -> bool {
        let Some(candidate) = self.mention_autocomplete.selected_candidate().cloned() else {
            return false;
        };
        if !candidate.is_directory() {
            return self.insert_selected_mention();
        }

        let start = self.mention_autocomplete.active_start;
        let end = self.mention_autocomplete.active_end;
        let replacement = format!("@{}/", candidate.target.trim_end_matches('/'));
        let start_byte = char_to_byte(&self.input, start);
        let end_byte = char_to_byte(&self.input, end);
        self.input.replace_range(start_byte..end_byte, &replacement);

        let old_len = end.saturating_sub(start);
        let new_len = replacement.chars().count();
        self.apply_input_edit(start, old_len, new_len);
        self.cursor_char = start + new_len;
        self.ensure_input_cursor_visible();
        self.autocomplete.visible = false;
        self.mention_autocomplete
            .update(&self.input, self.cursor_char);
        true
    }

    pub fn cancel_mention_autocomplete(&mut self) {
        self.mention_autocomplete.visible = false;
        self.mention_autocomplete.clear_session_cache();
    }

    pub fn clear_input(&mut self) -> bool {
        if self.input.is_empty() {
            return false;
        }

        self.input.clear();
        self.input_mentions.clear();
        self.input_images.clear();
        self.input_paste_markers.clear();
        self.cursor_char = 0;
        self.input_scroll_line = 0;
        self.autocomplete.visible = false;
        self.mention_autocomplete.visible = false;
        self.mention_autocomplete.clear_session_cache();
        true
    }

    pub fn take_input_draft(&mut self) -> Option<UserDraft> {
        if self.input.is_empty() {
            return None;
        }

        let text = std::mem::take(&mut self.input);
        let mentions = std::mem::take(&mut self.input_mentions);
        let images = std::mem::take(&mut self.input_images);
        let paste_markers = std::mem::take(&mut self.input_paste_markers);
        self.cursor_char = 0;
        self.input_scroll_line = 0;
        self.autocomplete.visible = false;
        self.mention_autocomplete.visible = false;
        self.mention_autocomplete.clear_session_cache();

        let (text, mentions) = expand_paste_markers(text, mentions, paste_markers);
        Some(UserDraft {
            text,
            mentions: mentions.iter().map(InputMention::display_mention).collect(),
            images: images
                .iter()
                .map(InputImageAttachment::display_attachment)
                .collect(),
        })
    }

    fn apply_input_edit(&mut self, start: usize, old_len: usize, new_len: usize) {
        let end = start + old_len;
        let delta = new_len as isize - old_len as isize;
        self.input_mentions.retain_mut(|mention| {
            if mention.end_char <= start {
                true
            } else if mention.start_char >= end {
                mention.start_char = shift_char(mention.start_char, delta);
                mention.end_char = shift_char(mention.end_char, delta);
                true
            } else {
                false
            }
        });
        self.input_images.retain_mut(|image| {
            if image.end_char <= start {
                true
            } else if image.start_char >= end {
                image.start_char = shift_char(image.start_char, delta);
                image.end_char = shift_char(image.end_char, delta);
                true
            } else {
                false
            }
        });
        self.input_paste_markers.retain_mut(|marker| {
            if marker.end_char <= start {
                true
            } else if marker.start_char >= end {
                marker.start_char = shift_char(marker.start_char, delta);
                marker.end_char = shift_char(marker.end_char, delta);
                true
            } else {
                false
            }
        });
    }

    fn input_atom_before_cursor(&self) -> Option<(usize, usize)> {
        self.input_mentions
            .iter()
            .map(|mention| (mention.start_char, mention.end_char))
            .chain(
                self.input_images
                    .iter()
                    .map(|image| (image.start_char, image.end_char)),
            )
            .chain(
                self.input_paste_markers
                    .iter()
                    .map(|marker| (marker.start_char, marker.end_char)),
            )
            .filter(|(start, end)| self.cursor_char > *start && self.cursor_char <= *end)
            .max_by_key(|(start, _)| *start)
    }

    fn input_atom_after_cursor(&self) -> Option<(usize, usize)> {
        self.input_mentions
            .iter()
            .map(|mention| (mention.start_char, mention.end_char))
            .chain(
                self.input_images
                    .iter()
                    .map(|image| (image.start_char, image.end_char)),
            )
            .chain(
                self.input_paste_markers
                    .iter()
                    .map(|marker| (marker.start_char, marker.end_char)),
            )
            .filter(|(start, end)| self.cursor_char >= *start && self.cursor_char < *end)
            .min_by_key(|(start, _)| *start)
    }

    pub fn input_cursor_line_col(&self) -> Option<(usize, usize)> {
        let lines = self.input_visual_lines();
        for (line_idx, line) in lines.iter().enumerate() {
            if self.cursor_char >= line.start_char && self.cursor_char <= line.end_char {
                return Some((
                    line_idx,
                    self.input_display_width(line.start_char, self.cursor_char),
                ));
            }
        }
        lines.last().map(|line| {
            (
                lines.len().saturating_sub(1),
                self.input_display_width(line.start_char, line.end_char),
            )
        })
    }

    fn input_line_col_to_char(&self, target_line: usize, target_col: usize) -> usize {
        let Some(line) = self.input_visual_lines().get(target_line).copied() else {
            return self.input.chars().count();
        };

        let mut col = 0usize;
        for (idx, ch) in self
            .input
            .chars()
            .enumerate()
            .skip(line.start_char)
            .take(line.end_char.saturating_sub(line.start_char))
        {
            if col >= target_col {
                return idx;
            }
            col += char_display_width(ch);
        }
        line.end_char
    }

    pub fn input_line_bounds(&self) -> Vec<(usize, usize)> {
        self.input_visual_lines()
            .into_iter()
            .map(|line| (line.start_char, line.end_char))
            .collect()
    }

    pub fn input_visual_lines(&self) -> Vec<InputVisualLine> {
        let chars = self.input.chars().collect::<Vec<_>>();
        if chars.is_empty() {
            return vec![InputVisualLine {
                start_char: 0,
                end_char: 0,
            }];
        }

        let mut lines = Vec::new();
        let mut start = 0usize;
        let mut width = 0usize;
        for (idx, ch) in chars.iter().copied().enumerate() {
            if ch == '\n' {
                lines.push(InputVisualLine {
                    start_char: start,
                    end_char: idx,
                });
                start = idx + 1;
                width = 0;
                continue;
            }

            let char_width = char_display_width(ch);
            let capacity = self.input_visual_line_capacity(lines.len());
            if width > 0 && width + char_width > capacity {
                lines.push(InputVisualLine {
                    start_char: start,
                    end_char: idx,
                });
                start = idx;
                width = 0;
            }
            width += char_width;
        }
        lines.push(InputVisualLine {
            start_char: start,
            end_char: chars.len(),
        });
        lines
    }

    pub fn input_visual_line_prefix_width(&self, line_idx: usize) -> usize {
        let _ = line_idx;
        INPUT_PROMPT_WIDTH
    }

    pub fn input_display_width(&self, start: usize, end: usize) -> usize {
        self.input
            .chars()
            .skip(start)
            .take(end.saturating_sub(start))
            .map(char_display_width)
            .sum()
    }

    fn input_visual_line_capacity(&self, line_idx: usize) -> usize {
        self.input_wrap_width
            .saturating_sub(self.input_visual_line_prefix_width(line_idx))
            .max(1)
    }

    pub fn paste_marker_at(&self, start_char: usize) -> Option<&InputPasteMarker> {
        self.input_paste_markers
            .iter()
            .find(|marker| marker.start_char == start_char)
    }

    pub fn image_at(&self, start_char: usize) -> Option<&InputImageAttachment> {
        self.input_images
            .iter()
            .find(|image| image.start_char == start_char)
    }

    pub fn mention_at(&self, start_char: usize) -> Option<&InputMention> {
        self.input_mentions
            .iter()
            .find(|mention| mention.start_char == start_char)
    }

    fn delete_input_range(&mut self, start: usize, end: usize) {
        let start_byte = char_to_byte(&self.input, start);
        let end_byte = char_to_byte(&self.input, end);
        self.input.replace_range(start_byte..end_byte, "");
        self.apply_input_edit(start, end.saturating_sub(start), 0);
        self.ensure_input_cursor_visible();
    }
}

fn char_to_byte(input: &str, char_idx: usize) -> usize {
    input.chars().take(char_idx).map(char::len_utf8).sum()
}

fn shift_char(value: usize, delta: isize) -> usize {
    if delta.is_negative() {
        value.saturating_sub(delta.unsigned_abs())
    } else {
        value.saturating_add(delta as usize)
    }
}

fn char_display_width(ch: char) -> usize {
    UnicodeWidthChar::width(ch).unwrap_or(0)
}

fn unquote_single_pasted_path(text: &str) -> Option<&str> {
    let trimmed = text.trim();
    if trimmed.is_empty() || trimmed.contains('\n') || trimmed.contains('\r') {
        return None;
    }

    if let Some(stripped) = trimmed
        .strip_prefix('\'')
        .and_then(|value| value.strip_suffix('\''))
    {
        return Some(stripped);
    }
    if let Some(stripped) = trimmed
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
    {
        return Some(stripped);
    }
    Some(trimmed)
}

fn existing_image_path(path: PathBuf) -> Option<PathBuf> {
    if path.is_file() && is_supported_image_path(&path) {
        Some(path)
    } else {
        None
    }
}

fn is_supported_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "webp" | "gif"
            )
        })
        .unwrap_or(false)
}

fn expand_paste_markers(
    text: String,
    mut mentions: Vec<InputMention>,
    mut markers: Vec<InputPasteMarker>,
) -> (String, Vec<InputMention>) {
    if markers.is_empty() {
        return (text, mentions);
    }

    markers.sort_by_key(|item| item.start_char);
    mentions.sort_by_key(|item| item.start_char);

    let chars = text.chars().collect::<Vec<_>>();
    let mut expanded = String::new();
    let mut cursor = 0usize;
    let mut delta: isize = 0;
    let mut marker_iter = markers.iter().peekable();

    for mention in &mut mentions {
        while let Some(marker) = marker_iter.peek() {
            if marker.end_char > mention.start_char {
                break;
            }
            delta += marker.full_char_count as isize - marker.marker.chars().count() as isize;
            marker_iter.next();
        }
        mention.start_char = shift_char(mention.start_char, delta);
        mention.end_char = shift_char(mention.end_char, delta);
    }

    for marker in markers {
        for ch in &chars[cursor..marker.start_char] {
            expanded.push(*ch);
        }
        expanded.push_str(&marker.full_text);
        cursor = marker.end_char;
    }
    for ch in &chars[cursor..] {
        expanded.push(*ch);
    }

    (expanded, mentions)
}

pub(super) fn combined_user_draft(drafts: &[&UserDraft]) -> UserDraft {
    let mut text = String::new();
    let mut mentions = Vec::new();
    let mut images = Vec::new();
    let mut offset = 0usize;
    for (idx, draft) in drafts.iter().enumerate() {
        if idx > 0 {
            text.push('\n');
            offset += 1;
        }

        mentions.extend(draft.mentions.iter().map(|mention| DisplayMention {
            start_char: mention.start_char + offset,
            end_char: mention.end_char + offset,
            kind: mention.kind,
            label: mention.label.clone(),
            target: mention.target.clone(),
            description: mention.description.clone(),
        }));
        images.extend(draft.images.iter().map(|image| DisplayImageAttachment {
            start_char: image.start_char + offset,
            end_char: image.end_char + offset,
            marker: image.marker.clone(),
            source_path: image.source_path.clone(),
            file_name: image.file_name.clone(),
        }));
        offset += draft.text.chars().count();
        text.push_str(&draft.text);
    }

    UserDraft {
        text,
        mentions,
        images,
    }
}
