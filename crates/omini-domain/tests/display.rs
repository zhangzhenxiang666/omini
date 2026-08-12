use omini_domain::display::{
    DisplayImageAttachment, DisplayMention, HistoryItem, MentionKind, UserDraft, UserSubmission,
    referenced_context_text,
};
use omini_domain::message::{ContentBlock, Message, Role};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_DIR_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct TestTempDir {
    path: PathBuf,
}

impl TestTempDir {
    fn new() -> Self {
        let sequence = TEMP_DIR_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "omini-domain-display-test-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir(&path).expect("test temp directory should be created");
        Self { path }
    }

    fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.path.join(name);
        std::fs::write(&path, bytes).expect("test fixture should be written");
        path
    }
}

impl Drop for TestTempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[test]
fn plain_draft_builds_one_user_text_block_without_display_projection() {
    let draft = UserDraft::plain("hello".into());

    let submission = draft
        .clone()
        .into_submission()
        .expect("plain draft should build a submission");
    assert_eq!(
        submission,
        UserSubmission::plain(Message::from_user_text("hello".into()))
    );
    assert_eq!(
        draft.history_item(),
        HistoryItem::Message(Message::from_user_text("hello".into()))
    );
}

#[test]
fn referenced_context_keeps_non_command_mentions_in_input_order() {
    let mentions = vec![
        mention(
            MentionKind::Subagent,
            "reviewer",
            "review-agent",
            "Checks correctness",
        ),
        mention(MentionKind::File, "lib.rs", "src/lib.rs", "source"),
        mention(MentionKind::Command, "help", "help", "command"),
        mention(MentionKind::Directory, "src", "src", "directory"),
        mention(MentionKind::Subagent, "worker", "worker-agent", "   "),
    ];

    assert_eq!(
        referenced_context_text(&mentions),
        Some(concat!(
            "Referenced context:\n",
            "- Agent: @reviewer (Checks correctness). Use subagent \"review-agent\" if this helps answer the user.\n",
            "- File: src/lib.rs. Read this file if needed.\n",
            "- Directory: src. Inspect this directory if needed.\n",
            "- Agent: @worker. Use subagent \"worker-agent\" if this helps answer the user.\n",
        ).into())
    );
}

#[test]
fn empty_or_command_only_mentions_add_no_llm_context() {
    assert_eq!(referenced_context_text(&[]), None);
    assert_eq!(
        referenced_context_text(&[mention(MentionKind::Command, "help", "help", "command",)]),
        None
    );
}

#[test]
fn mentioned_draft_splits_llm_context_from_display_message() {
    let mentions = vec![
        mention(MentionKind::File, "lib.rs", "src/lib.rs", "source"),
        mention(MentionKind::Command, "help", "help", "command"),
    ];
    let draft = UserDraft {
        text: "check @lib.rs /help".into(),
        mentions: mentions.clone(),
        images: Vec::new(),
    };

    let submission = draft
        .clone()
        .into_submission()
        .expect("mentioned draft should build a submission");
    assert_eq!(submission.llm_message.role, Role::User);
    assert_eq!(
        submission.llm_message.content,
        vec![ContentBlock::from_text(
            concat!(
                "check @lib.rs /help\n\n",
                "Referenced context:\n",
                "- File: src/lib.rs. Read this file if needed.\n",
            )
            .into()
        )]
    );
    let display = submission
        .display_message
        .as_ref()
        .expect("mentions should create a display projection");
    assert_eq!(display.text, "check @lib.rs /help");
    assert_eq!(display.mentions, mentions);
    assert_eq!(draft.history_item(), HistoryItem::Display(display.clone()));
}

#[test]
fn submission_history_prefers_display_projection_when_present() {
    let message = Message::from_user_text("LLM context".into());
    let plain = UserSubmission::plain(message.clone());
    assert_eq!(plain.history_item(), HistoryItem::Message(message));

    let draft = UserDraft {
        text: "@reviewer please check".into(),
        mentions: vec![mention(
            MentionKind::Subagent,
            "reviewer",
            "review-agent",
            "Review",
        )],
        images: Vec::new(),
    };
    let submission = draft
        .into_submission()
        .expect("mentioned draft should build a submission");
    let expected = submission
        .display_message
        .clone()
        .expect("display projection should exist");
    assert_eq!(submission.history_item(), HistoryItem::Display(expected));
}

#[test]
fn supported_images_are_base64_encoded_and_sorted_by_source_position() {
    let temp = TestTempDir::new();
    let fixtures = [
        (30, "later.GIF", &[4_u8, 5][..], "image/gif", "BAU="),
        (10, "first.PNG", &[0_u8, 1, 2][..], "image/png", "AAEC"),
        (20, "middle.JpEg", &[3_u8][..], "image/jpeg", "Aw=="),
        (20, "same.webp", &[][..], "image/webp", ""),
    ];
    let images = fixtures
        .iter()
        .map(|(position, name, bytes, _, _)| image(*position, &temp.write(name, bytes)))
        .collect();

    let submission = UserDraft {
        text: "images".into(),
        mentions: Vec::new(),
        images,
    }
    .into_submission()
    .expect("supported images should build a submission");

    assert_eq!(submission.llm_message.content.len(), 5);
    assert_eq!(
        submission.llm_message.content[0],
        ContentBlock::from_text("images".into())
    );
    for (block, (_, _, _, media_type, data)) in submission.llm_message.content.iter().skip(1).zip([
        fixtures[1],
        fixtures[2],
        fixtures[3],
        fixtures[0],
    ]) {
        assert_eq!(
            block,
            &ContentBlock::from_base64_image(media_type.into(), data.into())
        );
    }
}

#[test]
fn jpeg_short_extension_is_supported() {
    let temp = TestTempDir::new();
    let path = temp.write("photo.jpg", &[255, 216, 255]);

    let submission = UserDraft {
        text: String::new(),
        mentions: Vec::new(),
        images: vec![image(0, &path)],
    }
    .into_submission()
    .expect("jpg image should be supported");

    assert_eq!(
        submission.llm_message.content[1],
        ContentBlock::from_base64_image("image/jpeg".into(), "/9j/".into())
    );
}

#[test]
fn invalid_image_paths_and_extensions_return_actionable_errors() {
    let temp = TestTempDir::new();
    let missing = temp.path.join("missing.png");
    let unsupported = temp.write("image.bmp", b"bmp");
    let extensionless = temp.write("image", b"raw");
    let directory = temp.path.join("directory.png");
    std::fs::create_dir(&directory).expect("fixture directory should be created");

    for (path, expected) in [
        (missing, "Image file does not exist"),
        (directory, "Image file does not exist"),
        (unsupported, "Unsupported image type"),
        (extensionless, "Unsupported image type"),
    ] {
        let error = UserDraft {
            text: "image".into(),
            mentions: Vec::new(),
            images: vec![image(0, &path)],
        }
        .into_submission()
        .expect_err("invalid image should reject the draft");
        assert!(
            error.contains(expected),
            "expected {expected:?} in error {error:?}"
        );
        assert!(error.contains(&path.to_string_lossy().into_owned()));
    }
}

#[test]
fn empty_display_metadata_is_omitted_from_json() {
    let mention = DisplayMention {
        start_char: 0,
        end_char: 4,
        kind: MentionKind::File,
        label: "file".into(),
        target: "src/lib.rs".into(),
        description: String::new(),
    };
    let attachment = DisplayImageAttachment {
        start_char: 5,
        end_char: 12,
        marker: "[image]".into(),
        source_path: "/tmp/image.png".into(),
        file_name: String::new(),
    };

    let mention_value = serde_json::to_value(mention).expect("mention should serialize");
    let attachment_value = serde_json::to_value(attachment).expect("attachment should serialize");
    assert_eq!(mention_value.get("description"), None);
    assert_eq!(attachment_value.get("file_name"), None);
    assert_eq!(mention_value["kind"], json!("file"));
}

fn mention(kind: MentionKind, label: &str, target: &str, description: &str) -> DisplayMention {
    DisplayMention {
        start_char: 0,
        end_char: label.chars().count(),
        kind,
        label: label.into(),
        target: target.into(),
        description: description.into(),
    }
}

fn image(start_char: usize, path: &Path) -> DisplayImageAttachment {
    DisplayImageAttachment {
        start_char,
        end_char: start_char + 1,
        marker: "[image]".into(),
        source_path: path.to_string_lossy().into_owned(),
        file_name: path
            .file_name()
            .expect("fixture path should have a file name")
            .to_string_lossy()
            .into_owned(),
    }
}
