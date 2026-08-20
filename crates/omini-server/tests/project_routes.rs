mod support;

use omini_protocol::{
    CreateProjectRequest, CreateThreadRequest, OpenProjectResponse, ProjectPathStatus,
    ProjectSummary, ProjectsResponse, ProtocolError, UpdateProjectRequest,
};
use reqwest::Method;
use uuid::Uuid;

async fn create_project(
    daemon: &support::TestDaemon,
    path: &std::path::Path,
    name: Option<&str>,
) -> ProjectSummary {
    let (status, project): (_, ProjectSummary) = daemon
        .send_json(
            Method::POST,
            "/projects",
            None,
            &CreateProjectRequest {
                path: path.display().to_string(),
                name: name.map(str::to_string),
            },
        )
        .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    project
}

#[tokio::test]
async fn projects_same_path_reuses_identity() {
    let mut daemon = support::TestDaemon::start("project-idempotency").await;
    let workspace = daemon.root().create_dir("workspace");

    let first = create_project(&daemon, &workspace, None).await;
    let second = create_project(&daemon, &workspace, Some("ignored name")).await;

    Uuid::parse_str(&first.id).expect("project ID should be a UUID");
    assert_eq!(first, second);
    assert_eq!(first.name, "workspace");
    assert_eq!(first.path, workspace.display().to_string());
    assert_eq!(first.path_status, ProjectPathStatus::Ready);
    assert!(!first.storage_key.is_empty());

    let (status, listed): (_, ProjectsResponse) = daemon.get("/projects").await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(
        listed,
        ProjectsResponse {
            projects: vec![first]
        }
    );

    daemon.shutdown().await;
}

#[tokio::test]
async fn projects_invalid_requests_reject() {
    let mut daemon = support::TestDaemon::start("project-invalid").await;

    let (status, error): (_, ProtocolError) = daemon
        .send_json(
            Method::POST,
            "/projects",
            None,
            &CreateProjectRequest {
                path: "   ".to_string(),
                name: None,
            },
        )
        .await;
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(error.code, "invalid_project");
    assert_eq!(error.message, "Project path cannot be empty");

    let workspace = daemon.root().create_dir("workspace");
    let project = create_project(&daemon, &workspace, None).await;
    let (status, error): (_, ProtocolError) = daemon
        .send_json(
            Method::PATCH,
            &format!("/projects/{}", project.id),
            None,
            &UpdateProjectRequest::default(),
        )
        .await;
    assert_eq!(status, reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(error.code, "invalid_project");
    assert_eq!(error.message, "Project update must include name or path");

    daemon.shutdown().await;
}

#[tokio::test]
async fn projects_relink_preserves_identity() {
    let mut daemon = support::TestDaemon::start("project-relink").await;
    let original_path = daemon.root().create_dir("original");
    let replacement_path = daemon.root().create_dir("replacement");
    let occupied_path = daemon.root().create_dir("occupied");
    let original = create_project(&daemon, &original_path, Some("Original")).await;
    let occupied = create_project(&daemon, &occupied_path, Some("Occupied")).await;

    let (status, relinked): (_, ProjectSummary) = daemon
        .send_json(
            Method::PATCH,
            &format!("/projects/{}", original.id),
            None,
            &UpdateProjectRequest {
                name: Some("Renamed".to_string()),
                path: Some(replacement_path.display().to_string()),
            },
        )
        .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(relinked.id, original.id);
    assert_eq!(relinked.storage_key, original.storage_key);
    assert_eq!(relinked.name, "Renamed");
    assert_eq!(relinked.path, replacement_path.display().to_string());
    assert_eq!(relinked.path_status, ProjectPathStatus::Ready);

    // 已登记路径不能同时属于两个项目，避免存储目录与 thread 归属分叉。
    let (status, error): (_, ProtocolError) = daemon
        .send_json(
            Method::PATCH,
            &format!("/projects/{}", original.id),
            None,
            &UpdateProjectRequest {
                name: None,
                path: Some(occupied_path.display().to_string()),
            },
        )
        .await;
    assert_eq!(status, reqwest::StatusCode::CONFLICT);
    assert_eq!(error.code, "project_conflict");
    assert!(error.message.contains(&occupied_path.display().to_string()));

    let (status, listed): (_, ProjectsResponse) = daemon.get("/projects").await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(
        listed,
        ProjectsResponse {
            projects: vec![occupied, relinked],
        }
    );

    daemon.shutdown().await;
}

#[tokio::test]
async fn projects_missing_path_rejects_open() {
    let mut daemon = support::TestDaemon::start("project-missing-path").await;
    let workspace = daemon.root().create_dir("workspace");
    let project = create_project(&daemon, &workspace, None).await;
    std::fs::remove_dir_all(&workspace).expect("workspace should be removable");

    let (status, listed): (_, ProjectsResponse) = daemon.get("/projects").await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(listed.projects.len(), 1);
    assert_eq!(listed.projects[0].id, project.id);
    assert_eq!(listed.projects[0].path_status, ProjectPathStatus::Missing);

    let (status, error): (_, ProtocolError) = daemon
        .send_json(
            Method::POST,
            &format!("/projects/{}/open", project.id),
            None,
            &serde_json::json!({}),
        )
        .await;
    assert_eq!(status, reqwest::StatusCode::CONFLICT);
    assert_eq!(error.code, "project_path_missing");
    assert_eq!(
        error.message,
        format!("Project path '{}' is not available", project.path)
    );

    daemon.shutdown().await;
}

#[tokio::test]
async fn projects_restart_restores_threads() {
    let mut daemon = support::TestDaemon::start("project-restart").await;
    let workspace = daemon.root().create_dir("workspace");
    let project = create_project(&daemon, &workspace, None).await;

    let (status, created): (_, omini_protocol::CreateThreadResponse) = daemon
        .send_json(
            Method::POST,
            &format!("/projects/{}/threads", project.id),
            None,
            &CreateThreadRequest::default(),
        )
        .await;
    assert_eq!(status, reqwest::StatusCode::OK);

    daemon.restart().await;

    let (status, opened): (_, OpenProjectResponse) = daemon
        .send_json(
            Method::POST,
            &format!("/projects/{}/open", project.id),
            None,
            &serde_json::json!({}),
        )
        .await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert_eq!(opened.project.id, project.id);
    assert_eq!(opened.project.storage_key, project.storage_key);
    assert_eq!(opened.threads.len(), 1);
    assert_eq!(opened.threads[0].id, created.thread_id);

    daemon.shutdown().await;
}
