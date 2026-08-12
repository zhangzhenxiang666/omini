#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_DIR_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub struct TestTempDir {
    path: PathBuf,
}

impl TestTempDir {
    pub fn new(label: &str) -> Self {
        loop {
            let sequence = TEMP_DIR_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "omini-config-{label}-{}-{sequence}",
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

pub const MINIMAL_CONFIG: &str = r#"
[providers.openai]
name = "OpenAI"
endpoint = "openai"
base_url = "https://openai.example"
api_key = "test-key"

[providers.openai.models.gpt-test]
name = "GPT Test"
"#;
