mod support;

use crate::support::store::*;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use omini_config::project::ThreadDir;
use omini_domain::display::DisplaySummary;
use omini_domain::message::{ContentBlock, ImageSource, ImageSourceType, Message, Role, TextBlock};
use omini_runtime_contract::persistence::RuntimePersistenceEvent;
use omini_server::{history, store::*};
use std::fs;

#[tokio::test]
// 数据库存引用，不保存图片 Base64；加载时须恢复等价 block。
async fn images_use_assets_and_round_trip() {
    let (db, project, _root) = temp_db().await;
    project.create_thread("t1").unwrap();
    db.create_thread(&test_thread("t1")).await.unwrap();
    let raw = b"not-really-a-png";
    let image = ContentBlock::Image(omini_domain::message::ImageBlock {
        source: ImageSource {
            source_type: ImageSourceType::Base64,
            media_type: "image/png".to_string(),
            data: BASE64_STANDARD.encode(raw),
        },
    });
    let message = Message::new(Role::User, vec![image.clone()]);
    db.append_llm_message("t1", &message, fixed_time(), &project.thread("t1"))
        .await
        .unwrap();
    let loaded = db
        .load_current_llm_messages("t1", &project.thread("t1"))
        .await
        .unwrap();
    assert_eq!(loaded, vec![message]);
    let db_content: String = sqlx::query_scalar("SELECT content FROM llm_messages LIMIT 1")
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert!(!db_content.contains(&BASE64_STANDARD.encode(raw)));
    let asset = fs::read_dir(project.thread("t1").assets_dir())
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert_eq!(fs::read(asset).unwrap(), raw);
}

#[tokio::test]
async fn compact_reuses_existing_media_asset() {
    let (db, project, _root) = temp_db().await;
    project.create_thread("t1").unwrap();
    db.create_thread(&test_thread("t1")).await.unwrap();
    let image_message = Message::new(
        Role::User,
        vec![ContentBlock::Image(omini_domain::message::ImageBlock {
            source: ImageSource {
                source_type: ImageSourceType::Base64,
                media_type: "image/png".to_string(),
                data: BASE64_STANDARD.encode(b"shared-image"),
            },
        })],
    );
    let thread_dir = project.thread("t1");
    db.append_llm_message("t1", &image_message, fixed_time(), &thread_dir)
        .await
        .unwrap();
    db.replace_llm_context(
        "t1",
        1,
        &[
            Message::from_user_text("summary".to_string()),
            image_message,
        ],
        fixed_time(),
        &thread_dir,
    )
    .await
    .unwrap();

    assert_eq!(fs::read_dir(thread_dir.assets_dir()).unwrap().count(), 1);
    let stored: Vec<String> = sqlx::query_scalar(
        "SELECT content FROM llm_messages
             WHERE thread_id = 't1' AND (
                 (context_version = 1 AND ordinal = 0) OR
                 (context_version = 2 AND ordinal = 1)
             ) ORDER BY context_version",
    )
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert_eq!(stored.len(), 2);
    assert_eq!(stored[0], stored[1]);
}

#[tokio::test]
// 大摘要外置后，UI 仍可恢复摘要且保留关联 model。
async fn large_summary_uses_sidecar() {
    let (db, project, _root) = temp_db().await;
    project.create_thread("t1").unwrap();
    db.create_thread(&test_thread("t1")).await.unwrap();
    let summary = DisplaySummary {
        id: "summary-1".to_string(),
        title: "Compacted".to_string(),
        markdown: "x".repeat(CONTENT_SIZE_THRESHOLD + 1),
        created_at: fixed_time(),
    };
    db.apply_persistence_event(
        &RuntimePersistenceEvent::InsertCompactSummaryMessage {
            thread_id: "t1".to_string(),
            summary: summary.clone(),
            model_ref: "provider/model".to_string(),
        },
        TEST_PROJECT_ID,
        &project,
    )
    .await
    .unwrap();

    let stored = db.get_messages("t1").await.unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].model_ref.as_deref(), Some("provider/model"));
    assert!(stored[0].content.contains("sidecar_document"));
    assert!(!stored[0].content.contains(&summary.markdown));
    assert_eq!(
        fs::read_dir(project.thread("t1").sidecars_dir())
            .unwrap()
            .count(),
        1
    );
    let loaded = history::load_messages(&db, "t1", &project.thread("t1")).await;
    assert_eq!(
        loaded,
        vec![omini_domain::display::HistoryItem::Summary(summary)]
    );
}

#[test]
fn threshold_is_strictly_greater_than_64_kib() {
    let root = TestRoot::new();
    let thread = ThreadDir::from_path(root.path.join("thread"));
    fs::create_dir_all(thread.path()).unwrap();
    let at_limit = ContentBlock::Text(TextBlock {
        text: "x".repeat(CONTENT_SIZE_THRESHOLD),
    });
    let prepared = prepare_blocks(&[at_limit], &thread).unwrap();
    assert_eq!(prepared.values[0]["type"], "text");
    let above_limit = ContentBlock::Text(TextBlock {
        text: "x".repeat(CONTENT_SIZE_THRESHOLD + 1),
    });
    let prepared = prepare_blocks(&[above_limit], &thread).unwrap();
    assert_eq!(prepared.values[0]["type"], "sidecar");
    let inline = prepare_blocks(
        &[ContentBlock::Text(TextBlock {
            text: "small".to_string(),
        })],
        &thread,
    )
    .unwrap();
    assert_eq!(inline.values[0]["type"], "text");
}
