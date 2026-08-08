use crate::event::replay::SequencedRuntimeEvent;
use crate::project::{ProjectManager, load_validated_config};
use crate::store::{self as store_model, Database};
use chrono::Utc;
use omini_config::OminiRoot;
use omini_config::project::ProjectDir;
use omini_protocol as client_proto;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::broadcast;

pub(super) const TEST_PROJECT_ID: &str = "550e8400-e29b-41d4-a716-446655440000";

pub(super) struct TestRoot {
    pub(super) path: PathBuf,
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

pub(super) fn unique_temp_root(test_name: &str) -> TestRoot {
    TestRoot {
        path: std::env::temp_dir().join(format!(
            "omini-server-{test_name}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        )),
    }
}

pub(super) fn write_config(root: &Path, include_extra_provider: bool) {
    fs::create_dir_all(root).expect("root should be created");
    let mut content = r#"
[providers.openai]
name = "OpenAI"
endpoint = "openai"
base_url = "https://openai.example"
api_key = "test-key"

[providers.openai.models.fast]
name = "Fast"
limit = 1000
thinking = false

[providers.openai.models.reasoner]
name = "Reasoner"
limit = 2000
thinking = true
"#
    .to_string();

    if include_extra_provider {
        content.push_str(
            r#"
[providers.anthropic]
name = "Anthropic"
endpoint = "anthropic"
base_url = "https://anthropic.example"
api_key = "anthropic-key"

[providers.anthropic.models.claude-test]
name = "Claude Test"
limit = 3000
thinking = true
"#,
        );
    }

    fs::write(root.join("config.toml"), content).expect("config should be written");
}

pub(super) fn write_project_config(cwd: &Path) {
    let project_config_dir = cwd.join(".omini");
    fs::create_dir_all(&project_config_dir).expect("project config dir should be created");
    fs::write(
        project_config_dir.join("config.toml"),
        r#"
[providers.anthropic]
name = "Anthropic"
endpoint = "anthropic"
base_url = "https://project-anthropic.example"
api_key = "project-anthropic-key"

[providers.anthropic.models.claude-project]
name = "Claude Project"
limit = 4000
thinking = true
"#,
    )
    .expect("project config should be written");
}

pub(super) async fn project_manager_for(root: &Path, cwd: &Path) -> (ProjectManager, ProjectDir) {
    write_config(root, false);
    fs::create_dir_all(cwd).expect("cwd should be created");
    let root = Arc::new(OminiRoot::from_path(root.to_path_buf()));
    let config = load_validated_config(&root, cwd).expect("config should load");
    let storage_key =
        omini_config::project::storage_key(cwd, uuid::Uuid::parse_str(TEST_PROJECT_ID).unwrap());
    let project = root
        .init_project(&storage_key, &config)
        .expect("project should initialize");
    let db_path = root.path().join("omini.sqlite");
    let db = Database::open(&db_path)
        .await
        .expect("database should open");
    let now = Utc::now();
    db.create_project(&store_model::Project {
        id: TEST_PROJECT_ID.to_string(),
        name: "test project".to_string(),
        path: cwd.display().to_string(),
        storage_key,
        created_at: now,
        updated_at: now,
        last_opened_at: None,
    })
    .await
    .expect("project should persist");
    (
        ProjectManager::new(
            TEST_PROJECT_ID.to_string(),
            root,
            cwd.to_path_buf(),
            project.clone(),
            Arc::new(db),
        ),
        project,
    )
}

pub(super) fn has_provider(providers: &[client_proto::ProviderInfo], provider: &str) -> bool {
    providers.iter().any(|candidate| candidate.id == provider)
}

pub(super) async fn recv_runtime_event_kind(
    events: &mut broadcast::Receiver<SequencedRuntimeEvent>,
    kind: &str,
) -> SequencedRuntimeEvent {
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        loop {
            let event = events
                .recv()
                .await
                .expect("runtime event should be broadcast");
            if event.event.kind() == kind {
                return event;
            }
        }
    })
    .await
    .expect("expected runtime event should arrive")
}

pub(super) fn test_thread(id: &str) -> store_model::Thread {
    let now = Utc::now();
    store_model::Thread {
        id: id.to_string(),
        project_id: TEST_PROJECT_ID.to_string(),
        parent_thread_id: None,
        spawn_tool_use_id: None,
        thread_type: "main".to_string(),
        agent_label: None,
        provider: "openai".to_string(),
        model: "gpt-test".to_string(),
        thinking_effort: None,
        title: Some(id.to_string()),
        current_context_tokens: 0,
        total_tokens: 0,
        total_cached_tokens: 0,
        llm_context_version: 1,
        created_at: now,
        updated_at: now,
    }
}
