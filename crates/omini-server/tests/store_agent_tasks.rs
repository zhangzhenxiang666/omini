mod support;

use crate::support::store::*;
use omini_domain::events::{AgentTaskResult, AgentTaskStatus};
use omini_domain::message::{ContentBlock, Message, Role};
use omini_server::history;

#[tokio::test]
// 创建失败不得留下子线程；重启后运行中任务必须按终态恢复。
async fn agent_task_creation_and_recovery() {
    let (db, project, _root) = temp_db().await;
    project.create_thread("owner").unwrap();
    db.create_thread(&test_thread("owner")).await.unwrap();
    let initial = Message::from_user_text("do work".to_string());

    let task = test_agent_task("task_running", "agent_running", "owner");
    db.create_agent_task(
        TEST_PROJECT_ID,
        &task,
        &test_agent_thread("agent_running", "owner"),
        &initial,
    )
    .await
    .unwrap();
    assert_eq!(db.get_messages("agent_running").await.unwrap().len(), 1);
    assert_eq!(
        db.load_current_llm_messages("agent_running", &project.thread("agent_running"))
            .await
            .unwrap(),
        vec![initial.clone()]
    );

    let cancelling = test_agent_task("task_cancelling", "agent_cancelling", "owner");
    db.create_agent_task(
        TEST_PROJECT_ID,
        &cancelling,
        &test_agent_thread("agent_cancelling", "owner"),
        &initial,
    )
    .await
    .unwrap();
    db.set_agent_tasks_cancelling(&["task_cancelling".to_string()], fixed_time())
        .await
        .unwrap();

    let invalid = test_agent_task("task_invalid", "agent_rolled_back", "missing_owner");
    assert!(
        db.create_agent_task(
            TEST_PROJECT_ID,
            &invalid,
            &test_agent_thread("agent_rolled_back", "owner"),
            &initial,
        )
        .await
        .is_err()
    );
    assert!(db.get_thread("agent_rolled_back").await.unwrap().is_none());

    db.initialize().await.unwrap();
    let tasks = db.list_agent_tasks("owner").await.unwrap();
    assert_eq!(tasks.len(), 2);
    assert_eq!(
        tasks
            .iter()
            .find(|task| task.task_id == "task_running")
            .unwrap()
            .status,
        AgentTaskStatus::Interrupted
    );
    assert_eq!(
        tasks
            .iter()
            .find(|task| task.task_id == "task_cancelling")
            .unwrap()
            .status,
        AgentTaskStatus::Cancelled
    );
    assert!(tasks.iter().all(|task| task.completed_at.is_some()));
}

#[tokio::test]
// 重复投递同一任务完成通知不能重复写入 UI 或 LLM 历史。
async fn agent_task_notification_is_idempotent() {
    let (db, project, _root) = temp_db().await;
    project.create_thread("owner").unwrap();
    db.create_thread(&test_thread("owner")).await.unwrap();
    let task = test_agent_task("task_done", "agent_done", "owner");
    db.create_agent_task(
        TEST_PROJECT_ID,
        &task,
        &test_agent_thread("agent_done", "owner"),
        &Message::from_user_text("do work".to_string()),
    )
    .await
    .unwrap();
    let completed_at = fixed_time();
    db.finish_agent_task(
        "task_done",
        AgentTaskStatus::Completed,
        &AgentTaskResult {
            output: Some("done".to_string()),
            error: None,
            warnings: Vec::new(),
        },
        completed_at,
    )
    .await
    .unwrap();
    let before = Message::new(
        Role::Assistant,
        vec![ContentBlock::from_text("before notification".to_string())],
    );
    db.append_llm_message("owner", &before, fixed_time(), &project.thread("owner"))
        .await
        .unwrap();
    let notification = omini_domain::display::AgentTaskNotification {
        tasks: vec![omini_domain::display::AgentTaskNotificationItem {
            task_id: "task_done".to_string(),
            agent: "general".to_string(),
            title: "Test agent".to_string(),
            status: AgentTaskStatus::Completed,
        }],
        created_at: completed_at,
    };
    let llm_message = Message::from_user_text("agent task completed".to_string());
    for _ in 0..2 {
        db.insert_agent_task_notification(
            "owner",
            &notification,
            &llm_message,
            &["task_done".to_string()],
            completed_at,
        )
        .await
        .unwrap();
    }
    let after = Message::new(
        Role::Assistant,
        vec![ContentBlock::from_text("after notification".to_string())],
    );
    db.append_llm_message("owner", &after, fixed_time(), &project.thread("owner"))
        .await
        .unwrap();

    let ui_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM messages WHERE thread_id = 'owner' AND kind = 'agent_task_notification'",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();
    let llm_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM llm_messages WHERE thread_id = 'owner' AND role = 'user'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(ui_count, 1);
    assert_eq!(llm_count, 1);
    assert_eq!(
        db.load_current_llm_messages("owner", &project.thread("owner"))
            .await
            .unwrap(),
        vec![before, llm_message, after]
    );
    assert!(db.list_agent_tasks("owner").await.unwrap()[0].notification_delivered);
    assert!(matches!(
        history::load_messages(&db, "owner", &project.thread("owner"))
            .await
            .as_slice(),
        [omini_domain::display::HistoryItem::AgentTaskNotification(restored)]
            if restored == &notification
    ));
}
