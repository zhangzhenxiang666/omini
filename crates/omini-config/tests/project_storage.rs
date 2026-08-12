mod support;

use chrono::{DateTime, Utc};
use omini_config::project::{ProjectDir, ProjectState, ProjectsDir, storage_key};
use omini_config::{ConfigError, UserConfig};
use omini_domain::config::ThinkingEffort;
use std::path::{Path, PathBuf};
use support::TestTempDir;
use uuid::Uuid;

fn fixed_time(value: &str) -> DateTime<Utc> {
    value.parse().expect("fixed timestamp should parse")
}

fn single_model_config() -> UserConfig {
    toml::from_str(
        r#"
[providers.openai]
endpoint = "openai"
base_url = "https://openai.example"
api_key = "key"

[providers.openai.models.gpt-test]
"#,
    )
    .expect("config fixture should parse")
}

#[test]
fn storage_key_sanitizes_path_and_keeps_uuid() {
    let id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
    let cases = [
        (
            Path::new("/workspace/rust_code/omini"),
            "-workspace-rust-code-omini--550e8400-e29b-41d4-a716-446655440000",
        ),
        (
            Path::new("a_b/c d"),
            "a-b-c-d--550e8400-e29b-41d4-a716-446655440000",
        ),
        (
            Path::new(""),
            "project--550e8400-e29b-41d4-a716-446655440000",
        ),
    ];

    for (path, expected) in cases {
        assert_eq!(storage_key(path, id), expected);
    }
}

#[test]
fn uuid_separates_colliding_readable_prefixes() {
    let left = storage_key(Path::new("/tmp/a_b"), Uuid::from_u128(1));
    let right = storage_key(Path::new("/tmp/a-b"), Uuid::from_u128(2));

    assert!(left.starts_with("-tmp-a-b--"));
    assert!(right.starts_with("-tmp-a-b--"));
    assert_ne!(left, right);
    assert!(left.ends_with("--00000000-0000-0000-0000-000000000001"));
    assert!(right.ends_with("--00000000-0000-0000-0000-000000000002"));
}

#[test]
fn unicode_storage_key_respects_byte_limit() {
    let id = Uuid::from_u128(3);
    let path = format!("/项目/{}", "目录".repeat(100));

    let key = storage_key(Path::new(&path), id);

    // 截断只作用于可读前缀，必须同时守住 UTF-8 边界和完整 UUID 唯一性后缀。
    assert!(key.len() <= 240);
    assert!(key.contains('目'));
    assert!(key.ends_with(&format!("--{id}")));
}

#[test]
fn project_and_thread_paths_are_derived_without_io() {
    let projects = ProjectsDir::new(Path::new("/state/root"));
    let project = ProjectDir::from_path(PathBuf::from("/state/root/projects/p1"));
    let thread = project.thread("t1");

    assert_eq!(projects.path(), Path::new("/state/root/projects"));
    assert_eq!(project.path(), Path::new("/state/root/projects/p1"));
    assert_eq!(
        project.state_path(),
        PathBuf::from("/state/root/projects/p1/state.toml")
    );
    assert_eq!(
        project.threads_dir(),
        PathBuf::from("/state/root/projects/p1/threads")
    );
    assert_eq!(
        thread.path(),
        Path::new("/state/root/projects/p1/threads/t1")
    );
    assert_eq!(thread.assets_dir(), thread.path().join("assets"));
    assert_eq!(thread.sidecars_dir(), thread.path().join("sidecars"));
    assert_eq!(thread.staging_dir(), thread.path().join("staging"));
}

#[test]
fn thread_creation_is_idempotent_and_complete() {
    let temp = TestTempDir::new("thread-layout");
    let project_path = temp.create_dir("project");
    let project = ProjectDir::from_path(project_path);

    let first = project
        .create_thread("t1")
        .expect("thread should be created");
    temp.write("project/threads/t1/assets/existing.bin", b"data");
    let second = project
        .create_thread("t1")
        .expect("repeated creation should be idempotent");

    assert_eq!(first.path(), second.path());
    assert_eq!(first.path(), project.threads_dir().join("t1"));
    assert!(first.assets_dir().is_dir());
    assert!(first.sidecars_dir().is_dir());
    assert!(first.staging_dir().is_dir());
    assert_eq!(
        std::fs::read(first.assets_dir().join("existing.bin")).unwrap(),
        b"data"
    );
}

#[test]
fn thread_listing_returns_directories_only() {
    let temp = TestTempDir::new("thread-list");
    let project_path = temp.create_dir("project");
    let project = ProjectDir::from_path(project_path);
    assert!(project.list_threads().unwrap().is_empty());
    project.create_thread("second").unwrap();
    project.create_thread("first").unwrap();
    temp.write("project/threads/not-a-thread.txt", "ignored");

    let mut names = project
        .list_threads()
        .expect("threads should list")
        .into_iter()
        .map(|thread| thread.path().file_name().unwrap().to_owned())
        .collect::<Vec<_>>();
    names.sort();

    // read_dir 不承诺顺序，公开契约是完整目录集合而不是文件系统枚举顺序。
    assert_eq!(
        names,
        [
            std::ffi::OsString::from("first"),
            std::ffi::OsString::from("second")
        ]
    );
}

#[test]
fn missing_state_returns_defaults_without_creating_file() {
    let temp = TestTempDir::new("state-defaults");
    let project = ProjectDir::from_path(temp.path().join("missing-project"));

    let state = project
        .load_state()
        .expect("missing state should use defaults");

    assert_eq!(state.default_provider, None);
    assert_eq!(state.default_model, None);
    assert_eq!(state.thinking_effort, None);
    assert!(state.show_thinking_blocks);
    assert_eq!(state.created_at, state.accessed_at);
    assert!(!project.state_path().exists());
}

#[test]
fn project_state_round_trip_preserves_exact_values() {
    let temp = TestTempDir::new("state-round-trip");
    let project_path = temp.create_dir("project");
    let project = ProjectDir::from_path(project_path);
    let state = ProjectState {
        default_provider: Some("anthropic".into()),
        default_model: Some("claude".into()),
        thinking_effort: Some(ThinkingEffort::XHigh),
        show_thinking_blocks: false,
        created_at: fixed_time("2020-01-02T03:04:05Z"),
        accessed_at: fixed_time("2021-02-03T04:05:06Z"),
    };

    project.save_state(&state).expect("state should save");
    let loaded = project.load_state().expect("state should load");

    assert_eq!(loaded.default_provider, state.default_provider);
    assert_eq!(loaded.default_model, state.default_model);
    assert_eq!(loaded.thinking_effort, state.thinking_effort);
    assert_eq!(loaded.show_thinking_blocks, state.show_thinking_blocks);
    assert_eq!(loaded.created_at, state.created_at);
    assert_eq!(loaded.accessed_at, state.accessed_at);
}

#[test]
fn legacy_state_without_display_flag_defaults_to_visible() {
    let temp = TestTempDir::new("state-default-field");
    let project_path = temp.create_dir("project");
    temp.write(
        "project/state.toml",
        r#"
default_provider = "openai"
default_model = "gpt-test"
thinking_effort = "high"
created_at = "2020-01-02T03:04:05Z"
accessed_at = "2020-01-02T03:04:06Z"
"#,
    );
    let project = ProjectDir::from_path(project_path);

    let state = project.load_state().expect("compatible state should load");

    assert_eq!(state.default_provider.as_deref(), Some("openai"));
    assert_eq!(state.default_model.as_deref(), Some("gpt-test"));
    assert_eq!(state.thinking_effort, Some(ThinkingEffort::High));
    assert!(state.show_thinking_blocks);
    assert_eq!(state.created_at, fixed_time("2020-01-02T03:04:05Z"));
    assert_eq!(state.accessed_at, fixed_time("2020-01-02T03:04:06Z"));
}

#[test]
fn first_project_open_seeds_model_state_and_layout() {
    let temp = TestTempDir::new("project-first-open");
    let projects = ProjectsDir::new(temp.path());

    let project = projects
        .for_storage_key("project-key", &single_model_config())
        .expect("project should initialize");
    let state = project.load_state().expect("initial state should load");

    assert_eq!(project.path(), temp.path().join("projects/project-key"));
    assert_eq!(state.default_provider.as_deref(), Some("openai"));
    assert_eq!(state.default_model.as_deref(), Some("gpt-test"));
    assert_eq!(state.thinking_effort, None);
    assert!(state.show_thinking_blocks);
    assert_eq!(state.created_at, state.accessed_at);
    assert!(project.state_path().is_file());
}

#[test]
fn reopening_project_preserves_choices_and_refreshes_access_time() {
    let temp = TestTempDir::new("project-reopen");
    let projects = ProjectsDir::new(temp.path());
    let project = projects
        .for_storage_key("project-key", &single_model_config())
        .expect("project should initialize");
    let fixed_created = fixed_time("2020-01-02T03:04:05Z");
    let fixed_accessed = fixed_time("2020-01-02T03:04:06Z");
    project
        .save_state(&ProjectState {
            default_provider: Some("custom-provider".into()),
            default_model: Some("custom-model".into()),
            thinking_effort: Some(ThinkingEffort::Max),
            show_thinking_blocks: false,
            created_at: fixed_created,
            accessed_at: fixed_accessed,
        })
        .unwrap();

    projects
        .for_storage_key("project-key", &single_model_config())
        .expect("existing project should reopen");
    let state = project.load_state().expect("refreshed state should load");

    assert_eq!(state.default_provider.as_deref(), Some("custom-provider"));
    assert_eq!(state.default_model.as_deref(), Some("custom-model"));
    assert_eq!(state.thinking_effort, Some(ThinkingEffort::Max));
    assert!(!state.show_thinking_blocks);
    assert_eq!(state.created_at, fixed_created);
    assert!(state.accessed_at > fixed_accessed);
}

#[test]
fn malformed_state_blocks_reopen_without_overwriting_file() {
    let temp = TestTempDir::new("project-malformed-state");
    let state_path = temp.write("projects/project-key/state.toml", "not = [valid");
    let projects = ProjectsDir::new(temp.path());
    let original = std::fs::read(&state_path).unwrap();

    let error = projects
        .for_storage_key("project-key", &single_model_config())
        .expect_err("malformed state should block reopen");

    assert!(matches!(error, ConfigError::TomlDe(_)));
    assert_eq!(std::fs::read(state_path).unwrap(), original);
}

#[test]
fn saving_state_without_project_directory_returns_io_error() {
    let temp = TestTempDir::new("state-save-failure");
    let project = ProjectDir::from_path(temp.path().join("missing-project"));
    let state = ProjectState {
        default_provider: None,
        default_model: None,
        thinking_effort: None,
        show_thinking_blocks: true,
        created_at: fixed_time("2020-01-02T03:04:05Z"),
        accessed_at: fixed_time("2020-01-02T03:04:05Z"),
    };

    let error = project
        .save_state(&state)
        .expect_err("missing parent should fail");

    assert!(matches!(
        error,
        ConfigError::Io(source) if source.kind() == std::io::ErrorKind::NotFound
    ));
    assert!(!project.state_path().exists());
}
