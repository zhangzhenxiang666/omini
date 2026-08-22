use crate::tools::{PendingToolPauses, ToolExecutionContext, ToolRegistry};
use crate::types::events::EngineToRuntimeEvent;
use omini_config::{CompactConfig, ModelTiers, ProviderProfile, Settings};
use omini_domain::config::{InputModality, ModelInfo, ProviderEndpointKind};
use omini_domain::events::ActiveProfile;
use omini_permissions::PermissionEngine;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{Notify, mpsc};

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

    pub fn write(&self, relative: &str, contents: impl AsRef<[u8]>) -> PathBuf {
        let path = self.path.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("test fixture parent should be created");
        }
        std::fs::write(&path, contents).expect("test fixture should be written");
        path
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

pub fn tool_context(cwd: &Path, tool_name: &str, image_input: bool) -> ToolExecutionContext {
    let (event_tx, _event_rx) = mpsc::channel::<EngineToRuntimeEvent>(8);
    let pending_tool_pauses: PendingToolPauses = Arc::new(Mutex::new(HashMap::new()));
    ToolExecutionContext {
        tool_use_id: format!("test-{tool_name}"),
        pause_id: format!("test-{tool_name}"),
        tool_name: tool_name.to_string(),
        settings: Arc::new(settings(cwd, image_input)),
        tool_registry: Arc::new(ToolRegistry::new()),
        event_tx,
        pending_tool_pauses: Arc::clone(&pending_tool_pauses),
        permission_engine: Arc::new(PermissionEngine::empty(cwd.to_path_buf())),
        active_profile: ActiveProfile::Main,
        cancelled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        cancel_notify: Arc::new(Notify::new()),
        runtime: None,
    }
}
