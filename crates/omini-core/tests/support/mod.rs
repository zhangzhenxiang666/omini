use omini_config::{CompactConfig, ModelTiers, ProviderProfile, Settings};
use omini_domain::config::{InputModality, ModelInfo, ProviderEndpointKind};
use std::collections::HashMap;
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

pub fn settings(cwd: &Path, image_input: bool) -> Settings {
    let provider = "test".to_string();
    let model = if image_input {
        "vision-model"
    } else {
        "text-model"
    }
    .to_string();
    let mut providers = HashMap::new();
    providers.insert(
        provider.clone(),
        ProviderProfile {
            name: "Test".to_string(),
            endpoint: ProviderEndpointKind::OpenAI,
            api_key: String::new(),
            base_url: url::Url::parse("http://127.0.0.1:9").unwrap(),
            models: vec![ModelInfo {
                id: model.clone(),
                name: None,
                limit: 256_000,
                thinking: false,
                input_modalities: image_input
                    .then_some(vec![InputModality::Text, InputModality::Image]),
                extra_body: None,
                extra_headers: None,
            }],
        },
    );
    Settings {
        api_key: String::new(),
        base_url: url::Url::parse("http://127.0.0.1:9").unwrap(),
        model,
        endpoint: ProviderEndpointKind::OpenAI,
        providers,
        active_provider: provider,
        system_prompt: None,
        language: None,
        max_turns: None,
        cwd: cwd.to_path_buf(),
        thinking_effort: None,
        permissions: None,
        compact: CompactConfig::default(),
        mcp_servers: HashMap::new(),
        model_tiers: ModelTiers::default(),
    }
}
