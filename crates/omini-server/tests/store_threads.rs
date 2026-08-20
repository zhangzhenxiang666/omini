mod support;

use crate::support::store::*;
use omini_domain::message::Message;
use omini_runtime_contract::persistence::ThreadRecord;
use omini_server::store::*;

#[test]
fn runtime_child_uses_server_project() {
    let now = fixed_time();
    let runtime = ThreadRecord {
        id: "child".to_string(),
        parent_thread_id: Some("parent".to_string()),
        spawn_tool_use_id: Some("tool".to_string()),
        thread_type: "agent".to_string(),
        agent_label: Some("explorer".to_string()),
        provider: "openai".to_string(),
        model: "gpt-test".to_string(),
        thinking_effort: None,
        title: Some("Explore".to_string()),
        current_context_tokens: 0,
        total_tokens: 0,
        total_cached_tokens: 0,
        llm_context_version: 1,
        created_at: now,
        updated_at: now,
    };

    let stored = thread_from_runtime(TEST_PROJECT_ID, &runtime);

    assert_eq!(stored.project_id, TEST_PROJECT_ID);
    assert_eq!(stored.id, "child");
    assert_eq!(stored.parent_thread_id.as_deref(), Some("parent"));
}

#[tokio::test]
async fn delete_thread_tree_cleans_rows_and_files() {
    let (db, project, _root) = temp_db().await;
    project.create_thread("parent").unwrap();
    project.create_thread("child").unwrap();
    db.create_thread(&test_thread("parent")).await.unwrap();
    let mut child = test_thread("child");
    child.parent_thread_id = Some("parent".to_string());
    child.thread_type = "agent".to_string();
    db.create_thread(&child).await.unwrap();
    db.append_llm_message(
        "child",
        &Message::from_user_text("x".repeat(CONTENT_SIZE_THRESHOLD + 1)),
        fixed_time(),
        &project.thread("child"),
    )
    .await
    .unwrap();

    db.delete_thread_tree("parent", &project).await.unwrap();

    assert!(db.get_thread("parent").await.unwrap().is_none());
    assert!(db.get_thread("child").await.unwrap().is_none());
    assert!(!project.thread("parent").path().exists());
    assert!(!project.thread("child").path().exists());
}
