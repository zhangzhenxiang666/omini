use crate::features::timeline::model::DisplayImageAttachment;

pub const PASTE_MARKER_THRESHOLD_CHARS: usize = 512;
pub const PASTE_MARKER_THRESHOLD_NEWLINES: usize = 2;
pub const MAX_INPUT_VISIBLE_LINES: usize = 3;
pub const DEFAULT_INPUT_WRAP_WIDTH: usize = 80;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputPasteMarker {
    pub start_char: usize,
    pub end_char: usize,
    pub marker: String,
    pub full_text: String,
    pub full_char_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputImageAttachment {
    pub start_char: usize,
    pub end_char: usize,
    pub marker: String,
    pub source_path: String,
    pub file_name: String,
}

impl InputImageAttachment {
    pub fn display_attachment(&self) -> DisplayImageAttachment {
        DisplayImageAttachment {
            start_char: self.start_char,
            end_char: self.end_char,
            marker: self.marker.clone(),
            source_path: self.source_path.clone(),
            file_name: self.file_name.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputVisualLine {
    pub start_char: usize,
    pub end_char: usize,
}
