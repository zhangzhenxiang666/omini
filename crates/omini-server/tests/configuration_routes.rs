mod support;

use omini_protocol::{
    BootstrapProjectConfigurationRequest, CreateProjectRequest, ProjectConfigurationResponse,
    ProjectConfigurationState, ProjectSummary, ProviderEndpointKind,
};
use reqwest::Method;

async fn register_project(daemon: &support::TestDaemon, workspace: &std::path::Path) -> String {
    let (status, project): (_, ProjectSummary) = daemon
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

#[tokio::test]
async fn unconfigured_project_bootstraps_without_restarting_daemon() {
    let mut daemon = support::TestDaemon::start_without_config("configuration-bootstrap").await;
    let workspace = daemon.root().create_dir("workspace");
    let project_id = register_project(&daemon, &workspace).await;

    let (status, initial): (_, ProjectConfigurationResponse) = daemon
        .get(&format!("/projects/{project_id}/configuration"))
        .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(
        initial.state,
        ProjectConfigurationState::SetupRequired,
        "{initial:?}"
    );
    assert_eq!(initial.code.as_deref(), Some("no_provider"));

    let response = daemon
        .client()
        .post(daemon.url(&format!("/projects/{project_id}/configuration")))
        .json(&BootstrapProjectConfigurationRequest {
            provider_id: "openai".to_string(),
            protocol: ProviderEndpointKind::OpenAI,
            base_url: "https://api.openai.com/v1".to_string(),
            model_id: "gpt-5".to_string(),
            environment_variable: Some("OPENAI_API_KEY".to_string()),
            api_key: Some("test-secret".to_string()),
        })
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.text().await.unwrap();
    let config = std::fs::read_to_string(daemon.root().path().join(".omini/config.toml")).unwrap();
    assert_eq!(
        status,
        reqwest::StatusCode::OK,
        "body: {body}; config: {config}"
    );
    let configured: ProjectConfigurationResponse = serde_json::from_str(&body).unwrap();
    assert_eq!(configured.state, ProjectConfigurationState::Ready);

    let config = std::fs::read_to_string(daemon.root().path().join(".omini/config.toml")).unwrap();
    assert!(config.contains("env = \"OPENAI_API_KEY\""));
    let auth = std::fs::read_to_string(daemon.root().path().join(".omini/auth.json")).unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&auth).unwrap(),
        serde_json::json!({ "env": { "OPENAI_API_KEY": "test-secret" } })
    );

    daemon.shutdown().await;
}

#[tokio::test]
async fn invalid_configuration_is_reported_without_overwrite() {
    let mut daemon = support::TestDaemon::start("configuration-invalid").await;
    let workspace = daemon.root().create_dir("workspace");
    let project_id = register_project(&daemon, &workspace).await;
    daemon
        .root()
        .write(".omini/config.toml", "[providers.openai\n");

    let (status, response): (_, ProjectConfigurationResponse) = daemon
        .get(&format!("/projects/{project_id}/configuration"))
        .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(response.state, ProjectConfigurationState::Invalid);
    assert_eq!(response.code.as_deref(), Some("config_parse_error"));

    daemon.shutdown().await;
}
