mod support;

use crate::support::store::*;
use omini_domain::message::Message;
use omini_server::{history, store::*};
use std::fs;

#[tokio::test]
// 替换上下文只能切换当前版本，旧版本记录继续保留。
async fn llm_context_versions_are_immutable() {
    let (db, project, _root) = temp_db().await;
    project.create_thread("t1").unwrap();
    db.create_thread(&test_thread("t1")).await.unwrap();
    let first = Message::from_user_text("old".to_string());
    db.append_llm_message("t1", &first, fixed_time(), &project.thread("t1"))
        .await
        .unwrap();
    let next = vec![
        Message::from_user_text("summary".to_string()),
        first.clone(),
    ];
    assert_eq!(
        db.replace_llm_context("t1", 1, &next, fixed_time(), &project.thread("t1"))
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        db.load_current_llm_messages("t1", &project.thread("t1"))
            .await
            .unwrap(),
        next
    );
    let old_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM llm_messages WHERE thread_id = 't1' AND context_version = 1",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(old_count, 1);
}

#[tokio::test]
// agent 压缩推进 LLM context 版本，但不能重写已展示的 UI 历史。
async fn agent_compaction_preserves_ui_history() {
    let (db, project, _root) = temp_db().await;
    project.create_thread("owner").unwrap();
    db.create_thread(&test_thread("owner")).await.unwrap();
    let initial = Message::from_user_text("do work".to_string());
    let task = test_agent_task("task_compact", "agent_compact", "owner");
    db.create_agent_task(
        TEST_PROJECT_ID,
        &task,
        &test_agent_thread("agent_compact", "owner"),
        &initial,
    )
    .await
    .unwrap();
    let compacted = vec![
        Message::from_user_text("summary".to_string()),
        Message::from_user_text("retained tail".to_string()),
    ];

    let next_version = db
        .replace_llm_context(
            "agent_compact",
            1,
            &compacted,
            fixed_time(),
            &project.thread("agent_compact"),
        )
        .await
        .unwrap();

    assert_eq!(next_version, 2);
    assert_eq!(
        db.load_current_llm_messages("agent_compact", &project.thread("agent_compact"))
            .await
            .unwrap(),
        compacted
    );
    let old_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM llm_messages
                WHERE thread_id = 'agent_compact' AND context_version = 1",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(old_count, 1);
    assert!(matches!(
        history::load_messages(&db, "agent_compact", &project.thread("agent_compact"))
            .await
            .as_slice(),
        [omini_domain::display::HistoryItem::Message(message)] if message == &initial
    ));
}

#[tokio::test]
async fn context_conflict_cleans_sidecars() {
    let (db, project, _root) = temp_db().await;
    project.create_thread("t1").unwrap();
    db.create_thread(&test_thread("t1")).await.unwrap();
    let thread_dir = project.thread("t1");
    let message = Message::from_user_text("x".repeat(CONTENT_SIZE_THRESHOLD + 1));

    let error = db
        .replace_llm_context("t1", 99, &[message], fixed_time(), &thread_dir)
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        StoreError::ContextVersionConflict {
            expected: 99,
            actual: 1
        }
    ));
    assert_eq!(fs::read_dir(thread_dir.sidecars_dir()).unwrap().count(), 0);
    assert_eq!(fs::read_dir(thread_dir.staging_dir()).unwrap().count(), 0);
}
