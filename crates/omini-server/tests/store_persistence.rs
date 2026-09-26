mod support;

use crate::support::store::*;
use omini_domain::conversation::{ConversationEntry, UserInput};
use omini_domain::input::{InputPart, RunCommand, UserInputIntent};
use omini_domain::usage::Usage;
use omini_model::message::{ContentBlock, Message, Role};
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
    let display = UserInput {
        intent: UserInputIntent::Command {
            command: RunCommand::Init,
        },
        parts: vec![InputPart::Text {
            text: " notes".to_string(),
        }],
        attachments: Vec::new(),
    };
    let expanded = Message::from_user_text("INTERNAL EXPANDED INIT PROMPT".to_string());

    db.insert_user_input("typed", &display, fixed_time(), &project.thread("typed"))
        .await
        .unwrap();
    db.append_llm_message("typed", &expanded, fixed_time(), &project.thread("typed"))
        .await
        .unwrap();

    assert_eq!(
        history::load_messages(&db, "typed", &project.thread("typed")).await,
        vec![ConversationEntry::UserInput(display)]
    );
    assert_eq!(
        db.load_current_llm_messages("typed", &project.thread("typed"))
            .await
            .unwrap(),
        vec![expanded]
    );
}

#[tokio::test]
async fn tool_results_keep_their_timeline_and_model_context_positions() {
    let (db, project, _root) = temp_db().await;
    project.create_thread("tool-results").unwrap();
    db.create_thread(&test_thread("tool-results"))
        .await
        .unwrap();
    let thread_dir = project.thread("tool-results");
    let tool_use = Message::new(
        Role::Assistant,
        vec![ContentBlock::from_tool_use(
            "tool-1".to_string(),
            "read".to_string(),
            std::collections::HashMap::new(),
        )],
    );
    let tool_result = Message::new(
        Role::User,
        vec![ContentBlock::from_tool_result(
            "tool-1".to_string(),
            false,
            "file contents".to_string(),
        )],
    );
    let assistant_reply = Message::new(
        Role::Assistant,
        vec![ContentBlock::from_text("Done".to_string())],
    );

    for message in [&tool_use, &tool_result, &assistant_reply] {
        db.append_llm_message("tool-results", message, fixed_time(), &thread_dir)
            .await
            .unwrap();
    }
    for message in [&tool_use, &tool_result, &assistant_reply] {
        db.apply_persistence_event(
            &RuntimePersistenceEvent::UiMessageAppended {
                thread_id: "tool-results".to_string(),
                message: message.clone(),
                model_ref: (message.role == Role::Assistant).then(|| "provider/model".to_string()),
            },
            TEST_PROJECT_ID,
            &project,
        )
        .await
        .unwrap();
    }

    assert_eq!(
        db.load_current_llm_messages("tool-results", &thread_dir)
            .await
            .unwrap(),
        vec![tool_use, tool_result, assistant_reply]
    );
    assert!(matches!(
        history::load_messages(&db, "tool-results", &thread_dir)
            .await
            .as_slice(),
        [
            ConversationEntry::AssistantMessage(_),
            ConversationEntry::SystemEvent(
                omini_domain::conversation::SystemEvent::ToolResults { results }
            ),
            ConversationEntry::AssistantMessage(_)
        ] if results.len() == 1
            && results[0].tool_use_id == "tool-1"
            && results[0].content == "file contents"
    ));
}
