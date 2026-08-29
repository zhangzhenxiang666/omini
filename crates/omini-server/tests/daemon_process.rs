mod support;

use omini_protocol::{AckResponse, DaemonHealthResponse};
use serde_json::Value;
use std::process::Command;

#[test]
fn daemon_binary_unknown_argument_exits_with_usage_error() {
    let binary = std::env::var_os("CARGO_BIN_EXE_omini-server")
        .expect("Cargo should provide the omini-server test binary path");
    let output = Command::new(binary)
        .arg("--unknown")
        .output()
        .expect("daemon binary should run");

    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        String::from_utf8(output.stderr).expect("stderr should be UTF-8"),
        "unknown argument '--unknown'\n"
    );
}

#[tokio::test]
async fn daemon_foreground_startup_publishes_health_and_cleans_runtime_state() {
    let mut daemon = support::TestDaemon::start("foreground-lifecycle").await;

    let (status, health): (_, DaemonHealthResponse) = daemon.get("/health").await;
    assert_eq!(status, reqwest::StatusCode::OK);
    assert!(health.ok);
    assert_eq!(health.daemon, "omini-server");

    let state_path = daemon.root().path().join(".omini/run/daemon.json");
    let state: Value = serde_json::from_str(
        &std::fs::read_to_string(&state_path).expect("daemon state should be written"),
    )
    .expect("daemon state should be JSON");
    assert_eq!(state["host"], "127.0.0.1");
    assert!(state["port"].as_u64().is_some_and(|port| port > 0));
    assert!(state["pid"].as_u64().is_some_and(|pid| pid > 0));

    let response = daemon
        .client()
        .post(daemon.url("/shutdown"))
        .send()
        .await
        .expect("shutdown request should complete");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(
        response
            .json::<AckResponse>()
            .await
            .expect("shutdown response should decode"),
        AckResponse::ok()
    );
    daemon.wait_for_shutdown().await;

    assert!(!state_path.exists());
    assert!(!daemon.root().path().join(".omini/run/daemon.pid").exists());
}
