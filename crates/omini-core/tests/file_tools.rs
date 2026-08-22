mod support;

use omini_core::tools::Tool;
use omini_core::tools::edit_tool::{EditInput, EditTool, ReplaceError, replace};
use omini_core::tools::read_tool::{ReadInput, ReadTool};
use omini_core::tools::view_image_tool::{ViewImageInput, ViewImageTool};
use omini_core::tools::write_tool::{WriteInput, WriteTool};
use omini_domain::message::ContentBlock;

#[test]
fn edit_replace_handles_fallbacks_crlf_and_rejections() {
    assert_eq!(
        replace("  alpha\n", "alpha", "beta", false),
        Ok("  beta\n".into())
    );
    assert_eq!(
        replace("a\r\nb\r\n", "b", "B", false),
        Ok("a\r\nB\r\n".into())
    );
    assert_eq!(replace("x x x", "x", "y", true), Ok("y y y".into()));
    assert_eq!(
        replace("a a", "a", "b", false),
        Err(ReplaceError::MultipleMatches)
    );
    assert_eq!(
        replace("a", "", "b", false),
        Err(ReplaceError::EmptyOldString)
    );
    assert_eq!(replace("a", "a", "a", false), Err(ReplaceError::SameAsNew));
}

#[tokio::test]
// 该用例覆盖三个文件工具的公共路径约束与串联后的最终文件状态。
async fn file_tools_apply_and_reject_paths() {
    let temp = support::TestTempDir::new("file-tools");
    let file = temp.write("nested/note.txt", "first\nsecond\n");
    let path = file.display().to_string();

    let read = ReadTool
        .prepare(ReadInput {
            file_path: path.clone(),
            offset: None,
            limit: None,
        })
        .await
        .expect("absolute file should prepare");
    let read_result = ReadTool
        .execute_prepared(read, support::tool_context(temp.path(), "read", false))
        .await;
    assert!(!read_result.is_error);
    assert_eq!(read_result.output, "1: first\n2: second");
    assert_eq!(read_result.metadata, None);
    assert_eq!(read_result.extra_blocks, None);

    let edit = EditTool
        .prepare(EditInput {
            file_path: path.clone(),
            old_string: "second".into(),
            new_string: "SECOND".into(),
            replace_all: None,
        })
        .await
        .expect("unique edit should prepare");
    let edit_result = EditTool
        .execute_prepared(edit, support::tool_context(temp.path(), "edit", false))
        .await;
    assert!(!edit_result.is_error, "{}", edit_result.output);
    assert_eq!(
        std::fs::read_to_string(&file).expect("edited file should read"),
        "first\nSECOND\n"
    );

    let write_result = WriteTool
        .execute_prepared(
            WriteTool
                .prepare(WriteInput {
                    file_path: path,
                    content: "replacement\n".into(),
                })
                .await
                .expect("absolute write should prepare"),
            support::tool_context(temp.path(), "write", false),
        )
        .await;
    assert!(!write_result.is_error, "{}", write_result.output);
    assert_eq!(
        std::fs::read_to_string(&file).expect("written file should read"),
        "replacement\n"
    );

    for result in [
        ReadTool
            .prepare(ReadInput {
                file_path: "relative.txt".into(),
                offset: None,
                limit: None,
            })
            .await
            .err(),
        WriteTool
            .prepare(WriteInput {
                file_path: "relative.txt".into(),
                content: "x".into(),
            })
            .await
            .err(),
        EditTool
            .prepare(EditInput {
                file_path: "relative.txt".into(),
                old_string: "x".into(),
                new_string: "y".into(),
                replace_all: None,
            })
            .await
            .err(),
    ] {
        let error = result.expect("relative path should reject");
        assert!(error.is_error);
        assert_eq!(error.output, "file_path must be absolute: relative.txt");
    }
}

#[tokio::test]
async fn edit_changed_file_reports_error_without_overwriting_new_content() {
    let temp = support::TestTempDir::new("stale-edit");
    let file = temp.write("note.txt", "before\n");
    let prepared = EditTool
        .prepare(EditInput {
            file_path: file.display().to_string(),
            old_string: "before".into(),
            new_string: "after".into(),
            replace_all: None,
        })
        .await
        .expect("initial file should prepare");
    std::fs::write(&file, "changed\n").expect("fixture should change after preview");

    let result = EditTool
        .execute_prepared(prepared, support::tool_context(temp.path(), "edit", false))
        .await;
    assert!(result.is_error);
    assert_eq!(
        result.output.split_once(" in ").map(|(reason, _)| reason),
        Some("old_string not found")
    );
    assert_eq!(
        std::fs::read_to_string(&file).expect("changed file should read"),
        "changed\n"
    );
}

#[tokio::test]
async fn write_creates_parents_and_returns_complete_diff_metadata() {
    let temp = support::TestTempDir::new("write-diff");
    let file = temp.path().join("created/note.txt");
    let result = WriteTool
        .execute_prepared(
            WriteTool
                .prepare(WriteInput {
                    file_path: file.display().to_string(),
                    content: "alpha\nbeta\n".into(),
                })
                .await
                .expect("absolute path should prepare"),
            support::tool_context(temp.path(), "write", false),
        )
        .await;

    assert!(!result.is_error, "{}", result.output);
    assert_eq!(
        std::fs::read_to_string(&file).expect("created file should read"),
        "alpha\nbeta\n"
    );
    let metadata = result.metadata.expect("write should return diff metadata");
    assert_eq!(
        metadata.get("file_path"),
        Some(&serde_json::json!(file.display().to_string()))
    );
    let diff = metadata
        .get("diff")
        .and_then(|value| value.as_str())
        .expect("diff should be text");
    assert!(diff.starts_with("--- "));
    assert!(diff.contains("+alpha"));
    assert!(diff.contains("+beta"));
}

#[tokio::test]
// 仅模型声明图片输入能力时，工具结果才可附带图片块。
async fn view_image_requires_image_model() {
    let temp = support::TestTempDir::new("view-image");
    let file = temp.write("image.PNG", b"png-bytes");
    let prepared = ViewImageTool
        .prepare(ViewImageInput {
            path: file.display().to_string(),
        })
        .await
        .expect("supported image should prepare");

    let result = ViewImageTool
        .execute_prepared(
            prepared.clone(),
            support::tool_context(temp.path(), "view_image", true),
        )
        .await;
    assert_eq!(
        result.output,
        format!("Loaded image: {} (9 bytes, image/png)", file.display())
    );
    assert!(!result.is_error);
    assert_eq!(
        result.extra_blocks,
        Some(vec![ContentBlock::from_base64_image(
            "image/png".into(),
            "cG5nLWJ5dGVz".into()
        )])
    );

    let rejected = ViewImageTool
        .execute_prepared(
            prepared,
            support::tool_context(temp.path(), "view_image", false),
        )
        .await;
    assert!(rejected.is_error);
    assert_eq!(
        rejected.output,
        "view_image requires image input, but current model 'text-model' does not declare support for image input"
    );
}
