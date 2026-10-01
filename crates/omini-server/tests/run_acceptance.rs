mod support;

use omini_domain::agent_run::{AgentRunSnapshot, AgentRunStatus};
use omini_domain::conversation::UserInput;
use omini_server::store::MessageKind;
use support::store::*;

fn input(text: &str) -> UserInput {
    UserInput {
        intent: omini_domain::input::UserInputIntent::Message,
        parts: vec![omini_domain::input::InputPart::Text { text: text.into() }],
        attachments: Vec::new(),
    }
}

fn run() -> AgentRunSnapshot {
    AgentRunSnapshot {
        id: "accepted-run".into(),
        thread_id: "main".into(),
        parent_run_id: None,
        status: AgentRunStatus::Running,
        created_at: fixed_time(),
        started_at: Some(fixed_time()),
        finished_at: None,
        total_tokens: 0,
        archived_at: None,
    }
}

#[tokio::test]
async fn accept_run_input() {
    // 给定已存在的会话，当受理新运行，则展示历史与运行记录一起提交。
    let (db, project, _root) = temp_db().await;
    db.create_thread(&test_thread("main")).await.unwrap();
    let thread_dir = project.create_thread("main").unwrap();
    db.accept_run_input(&run(), &input("hello"), &thread_dir)
        .await
        .unwrap();
    assert_eq!(
        count_thread_messages(&db, "main", MessageKind::ConversationEntry).await,
        1
    );
    assert!(
        omini_server::store::AgentRun::filter_by_id("accepted-run")
            .first()
            .exec(&mut db.conn())
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn rollback_conflicting_run() {
    // 给定运行键已存在，当第二步运行写入冲突，则第一步展示消息也必须回滚。
    let (db, project, _root) = temp_db().await;
    db.create_thread(&test_thread("main")).await.unwrap();
    let thread_dir = project.create_thread("main").unwrap();
    db.create_agent_run(&run()).await.unwrap();
    assert!(
        db.accept_run_input(&run(), &input("rejected"), &thread_dir)
            .await
            .is_err()
    );
    assert_eq!(
        count_thread_messages(&db, "main", MessageKind::ConversationEntry).await,
        0
    );
}
