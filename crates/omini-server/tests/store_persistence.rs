mod support;

use crate::support::store::*;
use omini_domain::usage::Usage;
use omini_runtime_contract::persistence::RuntimePersistenceEvent;

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
