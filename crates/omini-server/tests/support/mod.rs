#![allow(dead_code)]

use reqwest::{Method, Response};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

static TEMP_DIR_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub const TEST_CONFIG: &str = r#"
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
"#;

pub struct TestTempDir {
    path: PathBuf,
}

impl TestTempDir {
    pub fn new(label: &str) -> Self {
        loop {
            let sequence = TEMP_DIR_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "omini-server-{label}-{}-{sequence}",
                std::process::id()
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => return Self { path },
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("test temp directory should be created: {error}"),
            }
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn create_dir(&self, relative: impl AsRef<Path>) -> PathBuf {
        let path = self.path.join(relative);
        std::fs::create_dir_all(&path).expect("test fixture directory should be created");
        path
    }

    pub fn write(&self, relative: impl AsRef<Path>, content: impl AsRef<[u8]>) -> PathBuf {
        let path = self.path.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("test fixture parent should be created");
        }
        std::fs::write(&path, content).expect("test fixture should be written");
        path
    }
}

impl Drop for TestTempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

pub struct TestDaemon {
    root: TestTempDir,
    child: Option<Child>,
    client: reqwest::Client,
    address: String,
}

impl TestDaemon {
    pub async fn start(label: &str) -> Self {
        let root = TestTempDir::new(label);
        root.write(".omini/config.toml", TEST_CONFIG);
        install_bundled_rg(&root);

        Self::start_with_root(root).await
    }

    pub async fn start_without_config(label: &str) -> Self {
        let root = TestTempDir::new(label);
        install_bundled_rg(&root);
        Self::start_with_root(root).await
    }

    async fn start_with_root(root: TestTempDir) -> Self {
        let child = spawn_daemon(&root);

        let client = reqwest::Client::builder()
            .build()
            .expect("HTTP client should build");
        let mut daemon = Self {
            root,
            child: Some(child),
            client,
            address: String::new(),
        };
        daemon.address = daemon.wait_for_address().await;
        daemon
    }

    pub fn root(&self) -> &TestTempDir {
        &self.root
    }

    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://{}/v1{path}", self.address)
    }

    pub fn websocket_url(&self, path: &str) -> String {
        format!("ws://{}/v1{path}", self.address)
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> (reqwest::StatusCode, T) {
        let response = self
            .client
            .get(self.url(path))
            .send()
            .await
            .expect("GET request should complete");
        decode(response).await
    }

    pub async fn send_json<B: Serialize, T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        client_id: Option<&str>,
        body: &B,
    ) -> (reqwest::StatusCode, T) {
        let mut request = self.client.request(method, self.url(path)).json(body);
        if let Some(client_id) = client_id {
            request = request.header("x-omini-client-id", client_id);
        }
        let response = request.send().await.expect("JSON request should complete");
        decode(response).await
    }

    pub async fn send_bytes(
        &self,
        path: &str,
        client_id: Option<&str>,
        mime_type: &str,
        body: Vec<u8>,
    ) -> Response {
        let mut request = self
            .client
            .post(self.url(path))
            .header("content-type", mime_type)
            .body(body);
        if let Some(client_id) = client_id {
            request = request.header("x-omini-client-id", client_id);
        }
        request
            .send()
            .await
            .expect("attachment request should complete")
    }

    pub async fn shutdown(&mut self) {
        let response = self
            .client
            .post(self.url("/shutdown"))
            .send()
            .await
            .expect("shutdown request should complete");
        assert!(
            response.status().is_success(),
            "shutdown should acknowledge"
        );

        self.wait_for_shutdown().await;
    }

    pub async fn restart(&mut self) {
        self.shutdown().await;
        self.child = Some(spawn_daemon(&self.root));
        self.address = self.wait_for_address().await;
    }

    pub async fn wait_for_shutdown(&mut self) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        loop {
            let status = self
                .child
                .as_mut()
                .expect("daemon child should exist")
                .try_wait()
                .expect("daemon status should be readable");
            if let Some(status) = status {
                assert!(
                    status.success(),
                    "daemon should exit successfully: {status}"
                );
                self.child.take();
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "daemon should stop after shutdown"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn wait_for_address(&mut self) -> String {
        let state_path = self.root.path().join(".omini/run/daemon.json");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        loop {
            if let Ok(content) = std::fs::read_to_string(&state_path)
                && let Ok(state) = serde_json::from_str::<Value>(&content)
                && let (Some(host), Some(port)) = (
                    state.get("host").and_then(Value::as_str),
                    state.get("port").and_then(Value::as_u64),
                )
            {
                let address = format!("{host}:{port}");
                if self
                    .client
                    .get(format!("http://{address}/v1/health"))
                    .send()
                    .await
                    .is_ok()
                {
                    return address;
                }
            }

            if let Some(status) = self
                .child
                .as_mut()
                .expect("daemon child should exist")
                .try_wait()
                .expect("daemon status should be readable")
            {
                panic!("daemon exited before becoming healthy: {status}");
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "daemon should become healthy within the fixed startup timeout"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

async fn decode<T: DeserializeOwned>(response: Response) -> (reqwest::StatusCode, T) {
    let status = response.status();
    let body = response
        .text()
        .await
        .expect("response body should be readable");
    let value = serde_json::from_str(&body).unwrap_or_else(|error| {
        panic!("response should be valid JSON ({status}): {error}; body: {body}")
    });
    (status, value)
}

fn spawn_daemon(root: &TestTempDir) -> Child {
    let binary = std::env::var_os("CARGO_BIN_EXE_omini-server")
        .expect("Cargo should provide the omini-server test binary path");
    Command::new(binary)
        .arg("--foreground")
        .env("HOME", root.path())
        .env("USERPROFILE", root.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("daemon process should start")
}

fn install_bundled_rg(root: &TestTempDir) {
    let paths = std::env::var_os("PATH")
        .map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
        .unwrap_or_default();
    let rg = paths
        .into_iter()
        .map(|directory| directory.join("rg"))
        .find(|path| path.is_file())
        .expect("test environment must provide rg on PATH");
    let target = root.path().join(".omini/bin/rg");
    std::fs::create_dir_all(target.parent().unwrap()).expect("test rg directory should be created");
    std::fs::copy(rg, target).expect("test rg should be copied");
}
pub mod store;
