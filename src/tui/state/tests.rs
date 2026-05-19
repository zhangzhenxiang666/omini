use super::*;
use crate::subagents::AgentSummary;
use crate::types::display::MentionKind;
use crate::types::events::{RuntimeToUiEvent, SubagentStartedEvent};
use crate::types::message::ToolResultBlock;

fn state_with_mention(cursor_char: usize) -> UiState {
    let mut state = UiState::new();
    state.input = "see @src now".to_string();
    state.cursor_char = cursor_char;
    state.input_mentions.push(InputMention {
        start_char: 4,
        end_char: 9,
        kind: MentionKind::Directory,
        label: "src".to_string(),
        target: "src".to_string(),
        description: "directory".to_string(),
    });
    state
}

fn long_paste_text() -> String {
    "x".repeat(PASTE_MARKER_THRESHOLD_CHARS + 1)
}

fn temp_image_path(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("omini_image_input_test_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, b"image").unwrap();
    path
}

fn start_subagent(state: &mut UiState) {
    state.apply_event(RuntimeToUiEvent::SubagentStarted(SubagentStartedEvent {
        session_id: "sub_1".to_string(),
        parent_session_id: "parent".to_string(),
        spawn_tool_use_id: "tool_1".to_string(),
        agent_label: "explorer".to_string(),
    }));
}

#[test]
fn subagent_spawn_tool_error_finishes_running_state() {
    let mut state = UiState::new();
    start_subagent(&mut state);

    state.apply_event(RuntimeToUiEvent::ToolResult(ToolResultBlock {
        tool_use_id: "tool_1".to_string(),
        is_error: true,
        content: "Stream error: Stream ended unexpectedly".to_string(),
        metadata: None,
    }));

    let node = state.subagents.get("sub_1").unwrap();
    assert_eq!(node.status, SubagentStatus::Failed);
}

#[test]
fn runtime_error_fails_running_subagent_state() {
    let mut state = UiState::new();
    start_subagent(&mut state);

    state.apply_event(RuntimeToUiEvent::Error(
        "Stream error: Stream ended unexpectedly".to_string(),
    ));

    let node = state.subagents.get("sub_1").unwrap();
    assert_eq!(node.status, SubagentStatus::Failed);
}

#[test]
fn agent_list_event_updates_subagent_mention_candidates() {
    let mut state = UiState::new();
    state.input = "@wo".to_string();
    state.cursor_char = 3;

    state.apply_event(RuntimeToUiEvent::AgentList(vec![
        AgentSummary {
            name: "explorer".to_string(),
            description: "Read-only codebase exploration agent.".to_string(),
        },
        AgentSummary {
            name: "worker".to_string(),
            description: "Implementation agent for focused coding tasks.".to_string(),
        },
    ]));

    assert!(state.mention_autocomplete.visible);
    let candidates: Vec<_> = state
        .mention_autocomplete
        .filtered
        .iter()
        .map(|candidate| {
            (
                candidate.kind,
                candidate.label.as_str(),
                candidate.target.as_str(),
                candidate.description.as_str(),
            )
        })
        .collect();
    assert_eq!(
        candidates,
        vec![(
            MentionKind::Subagent,
            "worker",
            "worker",
            "Implementation agent for focused coding tasks."
        )]
    );
}

#[test]
fn backspace_deletes_whole_mention_at_end() {
    let mut state = state_with_mention(9);
    state.delete_before();
    assert_eq!(state.input, "see now");
    assert_eq!(state.cursor_char, 4);
    assert!(state.input_mentions.is_empty());
}

#[test]
fn backspace_deletes_whole_mention_from_inside() {
    let mut state = state_with_mention(6);
    state.delete_before();
    assert_eq!(state.input, "see now");
    assert_eq!(state.cursor_char, 4);
    assert!(state.input_mentions.is_empty());
}

#[test]
fn delete_deletes_whole_mention_at_start() {
    let mut state = state_with_mention(4);
    state.delete_after();
    assert_eq!(state.input, "see now");
    assert_eq!(state.cursor_char, 4);
    assert!(state.input_mentions.is_empty());
}

#[test]
fn delete_deletes_whole_mention_from_inside() {
    let mut state = state_with_mention(6);
    state.delete_after();
    assert_eq!(state.input, "see now");
    assert_eq!(state.cursor_char, 4);
    assert!(state.input_mentions.is_empty());
}

#[test]
fn cursor_left_skips_whole_mention_at_end() {
    let mut state = state_with_mention(9);
    state.cursor_left();
    assert_eq!(state.cursor_char, 4);
}

#[test]
fn cursor_left_skips_whole_mention_from_inside() {
    let mut state = state_with_mention(6);
    state.cursor_left();
    assert_eq!(state.cursor_char, 4);
}

#[test]
fn cursor_right_skips_whole_mention_at_start() {
    let mut state = state_with_mention(4);
    state.cursor_right();
    assert_eq!(state.cursor_char, 9);
}

#[test]
fn cursor_right_skips_whole_mention_from_inside() {
    let mut state = state_with_mention(6);
    state.cursor_right();
    assert_eq!(state.cursor_char, 9);
}

#[test]
fn cursor_movement_in_plain_text_stays_character_based() {
    let mut state = state_with_mention(3);
    state.cursor_left();
    assert_eq!(state.cursor_char, 2);

    state.cursor_char = 9;
    state.cursor_right();
    assert_eq!(state.cursor_char, 10);
}

#[test]
fn inserted_mention_range_includes_trailing_space() {
    let mut state = UiState::new();
    state.input = "@sr".to_string();
    state.cursor_char = 3;
    state.mention_autocomplete.visible = true;
    state.mention_autocomplete.active_start = 0;
    state.mention_autocomplete.active_end = 3;
    state.mention_autocomplete.filtered.push(MentionCandidate {
        kind: MentionKind::Directory,
        label: "src".to_string(),
        target: "src".to_string(),
        description: "directory".to_string(),
    });

    assert!(state.insert_selected_mention());
    assert_eq!(state.input, "@src ");
    assert_eq!(state.cursor_char, 5);
    assert_eq!(state.input_mentions[0].start_char, 0);
    assert_eq!(state.input_mentions[0].end_char, 5);
}

#[test]
fn selected_image_mention_inserts_image_marker() {
    let image = temp_image_path("image.png");
    let cwd = image.parent().unwrap().to_path_buf();
    let mut state = UiState::new();
    state.status_bar.cwd = cwd;
    state.input = "@ima".to_string();
    state.cursor_char = 4;
    state.mention_autocomplete.visible = true;
    state.mention_autocomplete.active_start = 0;
    state.mention_autocomplete.active_end = 4;
    state.mention_autocomplete.filtered.push(MentionCandidate {
        kind: MentionKind::File,
        label: "image.png".to_string(),
        target: "image.png".to_string(),
        description: "file".to_string(),
    });

    assert!(state.insert_selected_mention());
    assert_eq!(state.input, "[Image#1] ");
    assert!(state.input_mentions.is_empty());
    assert_eq!(state.input_images.len(), 1);
    assert_eq!(state.input_images[0].source_path, image.to_string_lossy());
}

#[test]
fn quoted_existing_image_path_paste_inserts_image_marker() {
    let image = temp_image_path("dragged.jpg");
    let mut state = UiState::new();

    state.insert_paste(format!("'{}'", image.display()));

    assert_eq!(state.input, "[Image#1] ");
    assert_eq!(state.input_images.len(), 1);
    assert_eq!(state.input_images[0].source_path, image.to_string_lossy());
}

#[test]
fn nonexistent_image_path_paste_remains_text() {
    let mut state = UiState::new();
    let path = "/tmp/omini_missing_image.png";

    state.insert_paste(format!("'{path}'"));

    assert_eq!(state.input, format!("'{path}'"));
    assert!(state.input_images.is_empty());
}

#[test]
fn typed_quoted_existing_absolute_image_path_inserts_image_marker() {
    let image = temp_image_path("typed.png");
    let mut state = UiState::new();

    for ch in format!("'{}'", image.display()).chars() {
        state.insert_char(ch);
    }

    assert_eq!(state.input, "[Image#1] ");
    assert_eq!(state.input_images.len(), 1);
    assert_eq!(state.input_images[0].source_path, image.to_string_lossy());
}

#[test]
fn typed_quoted_image_path_with_spaces_inserts_image_marker() {
    let image = temp_image_path("typed image.png");
    let mut state = UiState::new();

    for ch in format!("\"{}\"", image.display()).chars() {
        state.insert_char(ch);
    }

    assert_eq!(state.input, "[Image#1] ");
    assert_eq!(state.input_images.len(), 1);
    assert_eq!(state.input_images[0].source_path, image.to_string_lossy());
}

#[test]
fn typed_quoted_nonexistent_image_path_remains_text() {
    let mut state = UiState::new();
    let text = "'/tmp/omini_missing_typed_image.png'";

    for ch in text.chars() {
        state.insert_char(ch);
    }

    assert_eq!(state.input, text);
    assert!(state.input_images.is_empty());
}

#[test]
fn typed_quoted_non_image_path_remains_text() {
    let file = temp_image_path("not-image.txt");
    let mut state = UiState::new();
    let text = format!("'{}'", file.display());

    for ch in text.chars() {
        state.insert_char(ch);
    }

    assert_eq!(state.input, text);
    assert!(state.input_images.is_empty());
}

#[test]
fn typed_at_text_without_selection_remains_plain_text() {
    let mut state = UiState::new();
    for c in "@src ".chars() {
        state.insert_char(c);
        state.update_input_autocomplete();
    }

    assert_eq!(state.input, "@src ");
    assert!(state.input_mentions.is_empty());

    state.cursor_left();
    assert_eq!(state.cursor_char, 4);
    state.delete_before();
    assert_eq!(state.input, "@sr ");
}

#[test]
fn short_paste_inserts_literal_newlines() {
    let mut state = UiState::new();
    state.insert_paste("one\ntwo".to_string());

    assert_eq!(state.input, "one\ntwo");
    assert!(state.input_paste_markers.is_empty());
    assert_eq!(state.input_line_count(), 2);
}

#[test]
fn paste_over_two_lines_inserts_marker_even_when_short() {
    let mut state = UiState::new();
    let pasted = "a\nb\nc".to_string();
    state.insert_paste(pasted.clone());

    assert_eq!(state.input, format!("[Pasted Content {} chars]", 5));
    assert_eq!(state.input_paste_markers.len(), 1);

    let draft = state.take_input_draft().unwrap();
    assert_eq!(draft.text, pasted);
}

#[test]
fn long_paste_inserts_marker_and_submit_expands_original_text() {
    let mut state = UiState::new();
    let pasted = long_paste_text();
    state.insert_paste(pasted.clone());

    assert_eq!(state.input_paste_markers.len(), 1);
    assert_eq!(
        state.input,
        format!(
            "[Pasted Content {} chars]",
            PASTE_MARKER_THRESHOLD_CHARS + 1
        )
    );

    let draft = state.take_input_draft().unwrap();
    assert_eq!(draft.text, pasted);
    assert!(draft.mentions.is_empty());
    assert!(state.input.is_empty());
    assert!(state.input_paste_markers.is_empty());
}

#[test]
fn cursor_skips_whole_paste_marker() {
    let mut state = UiState::new();
    state.insert_paste(long_paste_text());
    let marker_len = state.input.chars().count();

    state.cursor_left();
    assert_eq!(state.cursor_char, 0);

    state.cursor_right();
    assert_eq!(state.cursor_char, marker_len);
}

#[test]
fn delete_removes_whole_paste_marker() {
    let mut state = UiState::new();
    state.insert_paste(long_paste_text());
    state.cursor_home();
    state.delete_after();

    assert!(state.input.is_empty());
    assert!(state.input_paste_markers.is_empty());
}

#[test]
fn backspace_removes_whole_paste_marker() {
    let mut state = UiState::new();
    state.insert_paste(long_paste_text());
    state.delete_before();

    assert!(state.input.is_empty());
    assert!(state.input_paste_markers.is_empty());
    assert_eq!(state.cursor_char, 0);
}

#[test]
fn clear_input_resets_text_and_attachment_state() {
    let image = temp_image_path("clear.png");
    let mut state = UiState::new();
    state.status_bar.cwd = image.parent().unwrap().to_path_buf();
    state.insert_paste(long_paste_text());
    state.insert_char(' ');
    let mention_start = state.cursor_char;
    state.insert_text("@src ");
    state.input_mentions.push(InputMention {
        start_char: mention_start,
        end_char: mention_start + 5,
        kind: MentionKind::Directory,
        label: "src".to_string(),
        target: "src".to_string(),
        description: "directory".to_string(),
    });
    state.insert_image_attachment(image);
    state.autocomplete.visible = true;
    state.mention_autocomplete.visible = true;
    state.input_scroll_line = 1;

    assert!(state.clear_input());

    assert!(state.input.is_empty());
    assert!(state.input_mentions.is_empty());
    assert!(state.input_images.is_empty());
    assert!(state.input_paste_markers.is_empty());
    assert_eq!(state.cursor_char, 0);
    assert_eq!(state.input_scroll_line, 0);
    assert!(!state.autocomplete.visible);
    assert!(!state.mention_autocomplete.visible);
}

#[test]
fn clear_input_returns_false_when_input_is_empty() {
    let mut state = UiState::new();

    assert!(!state.clear_input());
}

#[test]
fn mention_offsets_shift_after_paste_marker_expansion() {
    let mut state = UiState::new();
    let pasted = long_paste_text();
    state.insert_paste(pasted.clone());
    state.insert_char(' ');
    let mention_start = state.cursor_char;
    state.insert_text("@src ");
    state.input_mentions.push(InputMention {
        start_char: mention_start,
        end_char: mention_start + 5,
        kind: MentionKind::Directory,
        label: "src".to_string(),
        target: "src".to_string(),
        description: "directory".to_string(),
    });

    let draft = state.take_input_draft().unwrap();
    assert_eq!(draft.text, format!("{pasted} @src "));
    assert_eq!(draft.mentions[0].start_char, pasted.chars().count() + 1);
    assert_eq!(draft.mentions[0].end_char, pasted.chars().count() + 6);
}

#[test]
fn input_visible_lines_caps_at_three_and_cursor_scrolls() {
    let mut state = UiState::new();
    state.insert_text("a\nb\nc\nd");

    assert_eq!(state.input_line_count(), 4);
    assert_eq!(state.input_visible_line_count(), 3);
    assert_eq!(state.input_scroll_line, 1);

    assert!(state.cursor_up_in_input());
    assert_eq!(state.input_scroll_line, 1);
    assert!(state.cursor_up_in_input());
    assert_eq!(state.input_scroll_line, 1);
    assert!(state.cursor_up_in_input());
    assert_eq!(state.input_scroll_line, 0);
}

#[test]
fn input_soft_wraps_by_width_without_mutating_text() {
    let mut state = UiState::new();
    state.set_input_wrap_width(6);
    state.insert_text("abcdefghi");

    assert_eq!(state.input, "abcdefghi");
    assert_eq!(state.input_line_bounds(), vec![(0, 4), (4, 8), (8, 9)]);
    assert_eq!(state.input_line_count(), 3);
    assert_eq!(state.input_visible_line_count(), 3);
}

#[test]
fn input_soft_wraps_wide_characters_by_display_width() {
    let mut state = UiState::new();
    state.set_input_wrap_width(6);
    state.insert_text("你好吗x");

    assert_eq!(state.input_line_bounds(), vec![(0, 2), (2, 4)]);
    assert_eq!(state.input_display_width(0, 2), 4);
    assert_eq!(state.input_display_width(2, 4), 3);
}

#[test]
fn input_soft_wrap_scrolls_after_three_visible_lines() {
    let mut state = UiState::new();
    state.set_input_wrap_width(6);
    state.insert_text("abcdefghijklmnopqrst");

    assert_eq!(
        state.input_line_bounds(),
        vec![(0, 4), (4, 8), (8, 12), (12, 16), (16, 20)]
    );
    assert_eq!(state.input_visible_line_count(), 3);
    assert_eq!(state.input_scroll_line, 2);
}

#[test]
fn cursor_moves_vertically_across_soft_wrapped_lines() {
    let mut state = UiState::new();
    state.set_input_wrap_width(6);
    state.insert_text("abcdefghijklmnopqrst");

    assert_eq!(state.input_cursor_line_col(), Some((4, 4)));
    assert!(state.cursor_up_in_input());
    assert_eq!(state.input_cursor_line_col(), Some((3, 4)));
    assert_eq!(state.cursor_char, 16);

    assert!(state.cursor_down_in_input());
    assert_eq!(state.input_cursor_line_col(), Some((4, 4)));
    assert_eq!(state.cursor_char, 20);
}

#[test]
fn manual_newlines_remain_real_line_breaks_with_soft_wrap() {
    let mut state = UiState::new();
    state.set_input_wrap_width(6);
    state.insert_text("ab\ncdefghi");

    assert_eq!(state.input, "ab\ncdefghi");
    assert_eq!(state.input_line_bounds(), vec![(0, 2), (3, 7), (7, 10)]);

    let draft = state.take_input_draft().unwrap();
    assert_eq!(draft.text, "ab\ncdefghi");
}
