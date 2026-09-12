mod support;

use crate::support::store::*;
use omini_domain::display::HistoryItem;
use omini_domain::input::{DisplayUserInput, InputPart, RunCommand, UserInputIntent};
use omini_domain::message::{Message, Role};
use omini_domain::usage::Usage;
use omini_runtime_contract::persistence::RuntimePersistenceEvent;
use omini_server::history;

#[tokio::test]
// agent 用量只累计 owner 总量，不覆盖主线程当前 context 用量。
async fn owner_agent_usage_updates_totals() {
    let (db, project, _root) = temp_db().await;
    project.create_thread("owner").unwrap();
    db.create_thread(&test_thread("owner")).await.unwrap();
    let main_usage = Usage {
        prompt_tokens: 8,
        completion_tokens: 2,
        cached_tokens: 1,
    };
    db.record_thread_usage("owner", main_usage).await.unwrap();
    let agent_usage = Usage {
        prompt_tokens: 4,
        completion_tokens: 1,
        cached_tokens: 2,
    };

    db.apply_persistence_event(
        &RuntimePersistenceEvent::RecordOwnerAgentUsage {
            thread_id: "owner".to_string(),
            usage: agent_usage,
        },
        TEST_PROJECT_ID,
        &project,
    )
    .await
    .unwrap();

    let owner = db.get_thread("owner").await.unwrap().unwrap();
    assert_eq!(owner.current_context_tokens, 10);
    assert_eq!(owner.total_tokens, 15);
    assert_eq!(owner.total_cached_tokens, 3);
}

#[tokio::test]
async fn typed_user_input_history_does_not_expose_expanded_llm_text() {
    let (db, project, _root) = temp_db().await;
    project.create_thread("typed").unwrap();
    db.create_thread(&test_thread("typed")).await.unwrap();
    let display = DisplayUserInput {
        role: Role::User,
        intent: UserInputIntent::Command {
            command: RunCommand::Init,
        },
        parts: vec![InputPart::Text {
            text: " notes".to_string(),
        }],
        attachments: Vec::new(),
    };
    let expanded = Message::from_user_text("INTERNAL EXPANDED INIT PROMPT".to_string());

    db.apply_persistence_event(
        &RuntimePersistenceEvent::InsertUserInput {
            thread_id: "typed".to_string(),
            display: display.clone(),
            created_at: fixed_time(),
        },
        TEST_PROJECT_ID,
        &project,
    )
    .await
    .unwrap();
    db.apply_persistence_event(
        &RuntimePersistenceEvent::AppendLlmMessage {
            thread_id: "typed".to_string(),
            message: expanded.clone(),
            created_at: fixed_time(),
        },
        TEST_PROJECT_ID,
        &project,
    )
    .await
    .unwrap();

    assert_eq!(
        history::load_messages(&db, "typed", &project.thread("typed")).await,
        vec![HistoryItem::UserInput(display)]
    );
    assert_eq!(
        db.load_current_llm_messages("typed", &project.thread("typed"))
            .await
            .unwrap(),
        vec![expanded]
    );
}
