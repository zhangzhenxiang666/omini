mod support;

use omini_protocol::{
    CreateProjectRequest, ModelsResponse, ProjectRuntimeConfigResponse, SetModelRequest,
    SetThinkingDisplayRequest, SetThinkingEffortRequest, ThinkingEffort,
};
use reqwest::Method;

const CONFIG_WITH_ANTHROPIC: &str = r#"
[providers.openai]
name = "OpenAI"
protocol = "openai"
base_url = "https://openai.example"
api_key = "test-key"

[providers.openai.models.fast]
name = "Fast"
context_window = 1000

thinking = false

[providers.openai.models.reasoner]
name = "Reasoner"
context_window = 2000

thinking = true

[providers.anthropic]
name = "Anthropic"
protocol = "anthropic"
base_url = "https://anthropic.example"
api_key = "anthropic-key"

[providers.anthropic.models.claude-test]
name = "Claude Test"
context_window = 3000

thinking = true
"#;

const PROJECT_CONFIG: &str = r#"
[providers.anthropic]
name = "Anthropic"
protocol = "anthropic"
base_url = "https://project-anthropic.example"
api_key = "project-anthropic-key"

[providers.anthropic.models.claude-project]
name = "Claude Project"
context_window = 4000

thinking = true
"#;

async fn register_project(daemon: &support::TestDaemon, workspace: &std::path::Path) -> String {
    let (status, project): (_, omini_protocol::ProjectSummary) = daemon
        .send_json(
            Method::POST,
            "/projects",
            None,
            &CreateProjectRequest {
                path: workspace.display().to_string(),
                name: None,
            },
        )
        .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    project.id
}

fn provider_ids(models: &ModelsResponse) -> Vec<&str> {
    models
        .providers
        .iter()
        .map(|provider| provider.id.as_str())
        .collect()
}

fn response_selects_listed_model(response: &ModelsResponse) -> bool {
    response.providers.iter().any(|provider| {
        provider.id == response.current_provider
            && provider
                .models
                .iter()
                .any(|model| model.id == response.current_model)
    })
}

#[tokio::test]
async fn project_models_refresh_after_config_change() {
    let mut daemon = support::TestDaemon::start("settings-refresh").await;
    let workspace = daemon.root().create_dir("workspace");
    let project_id = register_project(&daemon, &workspace).await;

    let (status, initial): (_, ModelsResponse) =
        daemon.get(&format!("/projects/{project_id}/models")).await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(provider_ids(&initial), vec!["openai"]);
    assert_eq!(initial.current_provider, "openai");
    // 未配置默认模型时，只承诺当前选择属于该 provider 的可用集合。
    assert!(response_selects_listed_model(&initial));

    daemon
        .root()
        .write(".omini/config.toml", CONFIG_WITH_ANTHROPIC);

    let (status, refreshed): (_, ModelsResponse) =
        daemon.get(&format!("/projects/{project_id}/models")).await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(provider_ids(&refreshed), vec!["anthropic", "openai"]);
    assert_eq!(refreshed.current_provider, "openai");
    assert!(response_selects_listed_model(&refreshed));

    daemon.shutdown().await;
}

#[tokio::test]
async fn project_models_merge_project_config() {
    let mut daemon = support::TestDaemon::start("settings-project-config").await;
    let workspace = daemon.root().create_dir("workspace");
    daemon
        .root()
        .write("workspace/.omini/config.toml", PROJECT_CONFIG);
    let project_id = register_project(&daemon, &workspace).await;

    let (status, models): (_, ModelsResponse) =
        daemon.get(&format!("/projects/{project_id}/models")).await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(provider_ids(&models), vec!["anthropic", "openai"]);
    let anthropic = &models.providers[0];
    assert_eq!(anthropic.id, "anthropic");
    assert_eq!(anthropic.models.len(), 1);
    assert_eq!(anthropic.models[0].id, "claude-project");
    assert_eq!(anthropic.models[0].limit, 4000);
    assert!(anthropic.models[0].thinking);

    daemon.shutdown().await;
}

#[tokio::test]
async fn project_model_without_thinking_clears_effort() {
    let mut daemon = support::TestDaemon::start("settings-model-selection").await;
    let workspace = daemon.root().create_dir("workspace");
    let project_id = register_project(&daemon, &workspace).await;

    let (status, selected): (_, ProjectRuntimeConfigResponse) = daemon
        .send_json(
            Method::POST,
            &format!("/projects/{project_id}/model"),
            None,
            &SetModelRequest {
                provider: "openai".to_string(),
                model: "fast".to_string(),
                thinking_effort: Some(ThinkingEffort::High),
            },
        )
        .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(
        selected,
        ProjectRuntimeConfigResponse {
            active_provider: "openai".to_string(),
            model: "fast".to_string(),
            thinking_effort: None,
            context_window: Some(1000),
            show_thinking_blocks: true,
        }
    );

    let (status, display): (_, ProjectRuntimeConfigResponse) = daemon
        .send_json(
            Method::POST,
            &format!("/projects/{project_id}/thinking-display"),
            None,
            &SetThinkingDisplayRequest { show: Some(false) },
        )
        .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert!(!display.show_thinking_blocks);
    assert_eq!(display.thinking_effort, None);

    daemon.shutdown().await;
}

#[tokio::test]
async fn project_thinking_none_effort_is_explicit() {
    let mut daemon = support::TestDaemon::start("settings-none-effort").await;
    let workspace = daemon.root().create_dir("workspace");
    let project_id = register_project(&daemon, &workspace).await;

    let (status, selected): (_, ProjectRuntimeConfigResponse) = daemon
        .send_json(
            Method::POST,
            &format!("/projects/{project_id}/model"),
            None,
            &SetModelRequest {
                provider: "openai".to_string(),
                model: "reasoner".to_string(),
                thinking_effort: Some(ThinkingEffort::High),
            },
        )
        .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(selected.thinking_effort, Some(ThinkingEffort::High));

    let (status, disabled): (_, ProjectRuntimeConfigResponse) = daemon
        .send_json(
            Method::POST,
            &format!("/projects/{project_id}/thinking-effort"),
            None,
            &SetThinkingEffortRequest {
                effort: ThinkingEffort::None,
            },
        )
        .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(disabled.thinking_effort, Some(ThinkingEffort::None));
    assert_eq!(disabled.context_window, Some(2000));

    daemon.shutdown().await;
}
