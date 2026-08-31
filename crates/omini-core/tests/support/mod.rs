use omini_config::{RawConfig, Settings};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_DIR_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub struct TestTempDir {
    path: PathBuf,
}

impl TestTempDir {
    pub fn new(label: &str) -> Self {
        let sequence = TEMP_DIR_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "omini-core-{label}-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("test temp directory should be created");
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestTempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[allow(dead_code)]
pub fn settings(cwd: &Path, image_input: bool) -> Settings {
    let model = if image_input {
        "vision-model"
    } else {
        "text-model"
    };
    let input = if image_input {
        r#"["text", "image"]"#
    } else {
        r#"["text"]"#
    };
    let raw: RawConfig = toml::from_str(&format!(
        r#"
[providers.test]
protocol = "openai"
base_url = "http://127.0.0.1:9"
api_key = "test-key"

[providers.test.models.{model}]
context_window = 256000
thinking = false
input = {input}
"#
    ))
    .expect("test config should parse");
    raw.resolve()
        .expect("test config should resolve")
        .to_settings(Some("test"), Some(model), None, cwd)
        .expect("test settings should build")
}
