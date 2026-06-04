use crate::api::LlmClient;
use crate::config::project::ProjectDir;
use crate::config::project::SessionDir;
use crate::engine::QueryEngine;
#[cfg(test)]
use crate::engine::ToolPauseResolver;
use crate::mcp::McpManager;
use crate::permissions::PermissionEngine;
use crate::persistence::RuntimePersistenceEvent;
use crate::subagents::RuntimeSubagentRunner;
use crate::tools::ToolRegistry;
use crate::types::config::Settings;
#[cfg(test)]
use crate::types::display::UserDraft;
use crate::types::display::{DisplayMessage, HistoryItem};
use crate::types::events::{
    ActiveProfile, RuntimeToUiEvent, SessionUsageSnapshot, UiToRuntimeEvent,
};
#[cfg(test)]
use crate::types::events::{
    EngineToRuntimeEvent, LoadedSession, PlanApprovalAction, ToolPauseKind, ToolPauseRequest,
    ToolPauseResponse,
};
use crate::types::message::Message;
use std::sync::Arc;
use std::sync::RwLock;
use std::sync::atomic::AtomicBool;
use tokio::sync::{Mutex, mpsc};
#[cfg(test)]
use uuid::Uuid;

#[cfg(test)]
use super::active_run;
use super::capabilities::CapabilityStore;
#[cfg(test)]
use super::manual_compact::persist_compact_summary_event;

pub(crate) struct RuntimeCapabilityHandles {
    pub(crate) mcp_manager: Arc<McpManager>,
    pub(crate) capabilities: Arc<CapabilityStore>,
}

impl RuntimeCapabilityHandles {
    pub(crate) fn load(settings: &Settings) -> Self {
        Self {
            mcp_manager: Arc::new(McpManager::from_settings(settings)),
            capabilities: Arc::new(CapabilityStore::load(settings)),
        }
    }
}

#[derive(Debug)]
pub(super) enum RunStart {
    /// 启动前将最新 runtime 消息同时写入 LLM 历史和 UI 历史。
    UserMessage,
    /// 启动前将最新 runtime 消息写入 JSONL，将 UI-only display 消息写入 SQLite/UI 历史。
    SplitDisplayMessage { display_message: DisplayMessage },
    /// 基于现有历史继续运行，不新增用户消息。
    Continue,
}

pub(super) fn initial_display_message(start: &RunStart) -> Option<HistoryItem> {
    match start {
        RunStart::SplitDisplayMessage { display_message } => {
            Some(HistoryItem::Display(display_message.clone()))
        }
        RunStart::UserMessage | RunStart::Continue => None,
    }
}

/// Agent 运行时。
///
/// 维护自己的对话历史，通过 channel 与 UI 双向通信。
/// 一次 `UiToRuntimeEvent::SendMessage` 可能触发多轮 LLM 调用和工具执行，
/// 直到 LLM 自然结束或达到最大轮次。
pub struct AgentRuntime {
    /// 当前会话 ID，第一次提交时生成。
    pub(crate) session_id: Option<String>,
    /// 创建后缓存的会话目录句柄。
    pub(crate) session_dir: Option<SessionDir>,
    /// 向 UI 发送事件。
    pub(super) event_tx: mpsc::Sender<RuntimeToUiEvent>,
    /// 向外部 server 转发 UI/SQLite 级持久化事件。
    pub(super) persistence_tx: mpsc::Sender<RuntimePersistenceEvent>,
    /// 接收 UI 发来的请求。
    pub(super) request_rx: mpsc::Receiver<UiToRuntimeEvent>,
    /// 运行时配置。
    pub(crate) settings: Settings,
    /// 当前项目目录。
    pub(crate) project: ProjectDir,
    /// 运行时自主维护的对话历史。
    pub(crate) messages: Vec<Message>,
    /// LLM 客户端。
    pub(super) llm_client: LlmClient,
    /// 查询引擎。
    pub(super) query_engine: QueryEngine,
    /// 工具注册表，持有所有注册的工具。
    pub(super) tool_registry: Arc<ToolRegistry>,
    /// 从用户配置加载的 MCP 服务管理器。
    pub(super) mcp_manager: Arc<McpManager>,
    /// runtime 是否已在 query 前等待过 MCP 启动。
    pub(super) mcp_initialized: bool,
    /// runtime 侧的子代理生命周期服务。
    pub(super) subagent_runner: Arc<RuntimeSubagentRunner>,
    /// runtime 管理的能力注册状态；每次 query 开始时生成只读快照。
    pub(super) capabilities: Arc<CapabilityStore>,
    /// 取消标志，用于 CancelRun。
    pub(super) cancelled: Arc<AtomicBool>,
    /// 当前活跃 profile，供 runtime 主循环和运行中事件处理器共享读取。
    pub(crate) active_profile: Arc<RwLock<ActiveProfile>>,
    /// 当前 session 的 usage 快照；SQLite 落库由 server 处理。
    pub(super) session_usage: Arc<Mutex<SessionUsageSnapshot>>,
}

impl AgentRuntime {
    pub fn new(
        event_tx: mpsc::Sender<RuntimeToUiEvent>,
        persistence_tx: mpsc::Sender<RuntimePersistenceEvent>,
        request_rx: mpsc::Receiver<UiToRuntimeEvent>,
        settings: Settings,
        project: ProjectDir,
    ) -> Self {
        Self::new_with_active_profile(
            event_tx,
            persistence_tx,
            request_rx,
            settings,
            project,
            ActiveProfile::Main,
        )
    }

    pub fn new_with_active_profile(
        event_tx: mpsc::Sender<RuntimeToUiEvent>,
        persistence_tx: mpsc::Sender<RuntimePersistenceEvent>,
        request_rx: mpsc::Receiver<UiToRuntimeEvent>,
        settings: Settings,
        project: ProjectDir,
        active_profile: ActiveProfile,
    ) -> Self {
        let handles = RuntimeCapabilityHandles::load(&settings);
        Self::with_capability_handles(
            event_tx,
            persistence_tx,
            request_rx,
            settings,
            project,
            handles,
            active_profile,
        )
    }

    pub(crate) fn with_capability_handles(
        event_tx: mpsc::Sender<RuntimeToUiEvent>,
        persistence_tx: mpsc::Sender<RuntimePersistenceEvent>,
        request_rx: mpsc::Receiver<UiToRuntimeEvent>,
        mut settings: Settings,
        project: ProjectDir,
        handles: RuntimeCapabilityHandles,
        active_profile: ActiveProfile,
    ) -> Self {
        let llm_client = LlmClient::new(
            settings.endpoint,
            settings.api_key.clone(),
            settings.base_url.clone(),
        );
        let tool_registry = Arc::new(crate::tools::create_main_registry());
        let subagent_runner = Arc::new(RuntimeSubagentRunner);
        let mcp_manager = handles.mcp_manager;
        let capabilities = handles.capabilities;
        let subagent_registry = capabilities.subagent_registry();
        let skill_registry = capabilities.skill_registry();
        settings.system_prompt = Some(crate::prompts::build_system_prompt_with_capabilities(
            &settings,
            &subagent_registry.summaries(),
            &skill_registry.injected_summaries(),
            active_profile,
        ));
        let permission_engine = Arc::new(PermissionEngine::load(
            settings.cwd.clone(),
            dirs::home_dir(),
            settings.permissions.clone(),
        ));

        for diagnostic in &subagent_registry.diagnostics {
            let _ = event_tx.try_send(RuntimeToUiEvent::warning(format!(
                "Subagent: {}",
                diagnostic.message()
            )));
        }
        for diagnostic in &skill_registry.diagnostics {
            let _ = event_tx.try_send(RuntimeToUiEvent::warning(format!(
                "Skill: {}",
                diagnostic.message()
            )));
        }
        for diagnostic in permission_engine.diagnostics() {
            let _ = event_tx.try_send(RuntimeToUiEvent::warning(format!(
                "Permission: {diagnostic}"
            )));
        }

        Self {
            session_id: None,
            session_dir: None,
            event_tx,
            persistence_tx,
            request_rx,
            settings,
            project,
            messages: Vec::new(),
            cancelled: Arc::new(AtomicBool::new(false)),
            llm_client,
            tool_registry,
            mcp_manager,
            mcp_initialized: false,
            subagent_runner,
            capabilities,
            query_engine: QueryEngine::new(permission_engine),
            active_profile: Arc::new(RwLock::new(active_profile)),
            session_usage: Arc::new(Mutex::new(SessionUsageSnapshot::default())),
        }
    }

    pub(crate) fn set_active_profile(&mut self, profile: ActiveProfile) {
        *self
            .active_profile
            .write()
            .expect("active profile lock poisoned") = profile;
        self.rebuild_system_prompt();
    }

    pub(crate) fn active_profile(&self) -> ActiveProfile {
        *self
            .active_profile
            .read()
            .expect("active profile lock poisoned")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::project::ProjectsDir;
    use crate::config::settings::{ModelEntry, ProviderConfig, UserConfig};
    use crate::types::config::{ProviderType, Settings};
    use crate::types::events::{NotificationKind, PlanExecutionProfile};
    use crate::types::message::{ContentBlock, Role};
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    async fn ensure_test_persistence() {}

    fn test_persistence_tx() -> mpsc::Sender<RuntimePersistenceEvent> {
        let (tx, mut rx) = mpsc::channel(256);
        tokio::spawn(async move { while rx.recv().await.is_some() {} });
        tx
    }

    fn test_persistence_channel() -> (
        mpsc::Sender<RuntimePersistenceEvent>,
        mpsc::Receiver<RuntimePersistenceEvent>,
    ) {
        mpsc::channel(256)
    }

    fn unique_temp_root(test_name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("omini-{test_name}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("failed to create temp test root");
        dir
    }

    fn test_user_config() -> UserConfig {
        let mut models = HashMap::new();
        models.insert(
            "gpt-test".to_string(),
            ModelEntry {
                name: None,
                limit: Some(256000),
                thinking: Some(true),
                input_modalities: None,
            },
        );

        let mut providers = HashMap::new();
        providers.insert(
            "openai".to_string(),
            ProviderConfig {
                name: Some("OpenAI".to_string()),
                endpoint: ProviderType::OpenAI,
                base_url: "https://openai.example".to_string(),
                api_key: "test-key".to_string(),
                models: Some(models),
            },
        );

        UserConfig {
            providers,
            language: None,
            permissions: None,
            compact: None,
            mcp_servers: HashMap::new(),
        }
    }

    fn settings_for_cwd(config: &UserConfig, cwd: &Path) -> Settings {
        let mut settings = config
            .to_settings(None, None, None)
            .expect("failed to build settings");
        settings.cwd = cwd.to_path_buf();
        settings
    }

    fn text_content(message: &Message) -> &str {
        let Some(ContentBlock::Text(text)) = message.content.first() else {
            panic!("expected first content block to be text");
        };
        &text.text
    }

    fn drain_events(event_rx: &mut mpsc::Receiver<RuntimeToUiEvent>) -> Vec<RuntimeToUiEvent> {
        let mut events = Vec::new();
        while let Ok(event) = event_rx.try_recv() {
            events.push(event);
        }
        events
    }

    fn permission_pause(tool_use_id: &str) -> ToolPauseRequest {
        ToolPauseRequest {
            tool_use_id: tool_use_id.to_string(),
            preview_tool_use_id: None,
            tool_name: "bash".to_string(),
            permission_source: None,
            source_session_id: None,
            source_agent_label: None,
            kind: ToolPauseKind::Permission(crate::types::events::PermissionPreview::Custom {
                tool_name: "bash".to_string(),
                payload: serde_json::Map::new(),
            }),
        }
    }

    fn empty_tool_pause_resolver() -> ToolPauseResolver {
        ToolPauseResolver::new(Arc::new(Mutex::new(HashMap::new())))
    }

    fn permission_tool_pause_resolver(
        tool_use_id: &str,
    ) -> (
        ToolPauseResolver,
        tokio::sync::oneshot::Receiver<ToolPauseResponse>,
    ) {
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let (tx, rx) = tokio::sync::oneshot::channel();
        pending
            .lock()
            .expect("pending tool pause mutex poisoned")
            .insert(
                tool_use_id.to_string(),
                crate::tools::PendingToolPause::Permission(tx),
            );
        (ToolPauseResolver::new(pending), rx)
    }

    #[tokio::test]
    async fn submit_user_message_emits_ui_echo_before_run_starts() {
        ensure_test_persistence().await;

        let root = unique_temp_root("submit-user-message-echo");
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).expect("failed to create cwd");
        let config = test_user_config();
        let project = ProjectsDir::new(&root)
            .for_cwd(&cwd, &config)
            .expect("failed to create project dir");
        let mut settings = settings_for_cwd(&config, &cwd);
        settings.max_turns = Some(0);
        let (event_tx, mut event_rx) = mpsc::channel(32);
        let (_request_tx, request_rx) = mpsc::channel(1);
        let mut runtime = AgentRuntime::new(
            event_tx,
            test_persistence_tx(),
            request_rx,
            settings,
            project,
        );
        drain_events(&mut event_rx);

        runtime
            .submit_user_message(UserDraft::plain("hello".to_string()))
            .await;

        let events = drain_events(&mut event_rx);
        let injected = events
            .iter()
            .position(|event| {
                matches!(
                    event,
                    RuntimeToUiEvent::UserMessageInjected(HistoryItem::Message(message))
                        if text_content(message) == "hello"
                )
            })
            .expect("user message should be echoed to UI");
        let started = events
            .iter()
            .position(|event| matches!(event, RuntimeToUiEvent::RunStarted))
            .expect("run should start");
        assert!(injected < started);
    }

    #[tokio::test]
    async fn toggle_active_profile_cycles_main_auto_plan() {
        let root = unique_temp_root("toggle-active-profile");
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).expect("failed to create cwd");
        let config = test_user_config();
        let project = ProjectsDir::new(&root)
            .for_cwd(&cwd, &config)
            .expect("failed to create project dir");
        let settings = settings_for_cwd(&config, &cwd);
        let (event_tx, mut event_rx) = mpsc::channel(16);
        let (_request_tx, request_rx) = mpsc::channel(1);
        let mut runtime = AgentRuntime::new(
            event_tx,
            test_persistence_tx(),
            request_rx,
            settings,
            project,
        );

        assert_eq!(runtime.active_profile(), ActiveProfile::Main);

        runtime.toggle_active_profile().await;
        assert_eq!(runtime.active_profile(), ActiveProfile::Auto);

        runtime.toggle_active_profile().await;
        assert_eq!(runtime.active_profile(), ActiveProfile::Plan);

        runtime.toggle_active_profile().await;
        assert_eq!(runtime.active_profile(), ActiveProfile::Main);

        let mut profiles = Vec::new();
        while let Ok(event) = event_rx.try_recv() {
            if let RuntimeToUiEvent::ActiveProfileChanged(profile) = event {
                profiles.push(profile);
            }
        }
        assert_eq!(
            profiles,
            vec![
                ActiveProfile::Auto,
                ActiveProfile::Plan,
                ActiveProfile::Main
            ]
        );
    }

    #[tokio::test]
    async fn active_run_profile_toggle_switches_main_and_auto_only() {
        let root = unique_temp_root("active-run-toggle-profile");
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).expect("failed to create cwd");
        let config = test_user_config();
        let project = ProjectsDir::new(&root)
            .for_cwd(&cwd, &config)
            .expect("failed to create project dir");
        let settings = settings_for_cwd(&config, &cwd);
        let (event_tx, mut event_rx) = mpsc::channel(16);
        let (_request_tx, request_rx) = mpsc::channel(1);
        let mut runtime = AgentRuntime::new(
            event_tx.clone(),
            test_persistence_tx(),
            request_rx,
            settings,
            project,
        );
        drain_events(&mut event_rx);

        let mut active_profile = runtime.active_profile();
        active_run::toggle_active_profile(
            &mut active_profile,
            &mut runtime.settings,
            &runtime.capabilities,
            &event_tx,
        )
        .await;
        assert_eq!(active_profile, ActiveProfile::Auto);

        active_run::toggle_active_profile(
            &mut active_profile,
            &mut runtime.settings,
            &runtime.capabilities,
            &event_tx,
        )
        .await;
        assert_eq!(active_profile, ActiveProfile::Main);

        let profiles: Vec<_> = drain_events(&mut event_rx)
            .into_iter()
            .filter_map(|event| match event {
                RuntimeToUiEvent::ActiveProfileChanged(profile) => Some(profile),
                _ => None,
            })
            .collect();
        assert_eq!(profiles, vec![ActiveProfile::Auto, ActiveProfile::Main]);

        runtime.set_active_profile(ActiveProfile::Plan);
        let mut active_profile = runtime.active_profile();
        active_run::toggle_active_profile(
            &mut active_profile,
            &mut runtime.settings,
            &runtime.capabilities,
            &event_tx,
        )
        .await;
        assert_eq!(active_profile, ActiveProfile::Plan);

        let profiles: Vec<_> = drain_events(&mut event_rx)
            .into_iter()
            .filter_map(|event| match event {
                RuntimeToUiEvent::ActiveProfileChanged(profile) => Some(profile),
                _ => None,
            })
            .collect();
        assert!(profiles.is_empty());
    }

    #[tokio::test]
    async fn proposed_plan_block_is_persisted_as_submitted_plan() {
        ensure_test_persistence().await;

        let root = unique_temp_root("proposed-plan");
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).expect("failed to create cwd");
        let config = test_user_config();
        let project = ProjectsDir::new(&root)
            .for_cwd(&cwd, &config)
            .expect("failed to create project dir");
        let settings = settings_for_cwd(&config, &cwd);
        let (event_tx, _event_rx) = mpsc::channel(16);
        let (_request_tx, request_rx) = mpsc::channel(1);
        let mut runtime = AgentRuntime::new(
            event_tx,
            test_persistence_tx(),
            request_rx,
            settings,
            project.clone(),
        );

        runtime.set_active_profile(ActiveProfile::Plan);
        runtime.messages = vec![Message::new(
            Role::Assistant,
            vec![ContentBlock::from_text(
                "Intro\n<proposed_plan>\n# Durable Plan\n\n- Execute it.\n</proposed_plan>\nOutro"
                    .to_string(),
            )],
        )];

        let plan = runtime
            .persist_latest_proposed_plan()
            .await
            .expect("persist proposed plan should succeed")
            .expect("plan should be extracted");

        assert_eq!(plan.title, "Durable Plan");
        assert_eq!(plan.id, "plan");
        assert_eq!(plan.markdown, "# Durable Plan\n\n- Execute it.");
        assert_eq!(plan.path, project.path().join("plans").join("plan.md"));
        assert_eq!(
            std::fs::read_to_string(&plan.path).expect("plan file should exist"),
            plan.markdown
        );
    }

    #[tokio::test]
    async fn proposed_plan_overwrites_current_plan_file() {
        ensure_test_persistence().await;

        let root = unique_temp_root("proposed-plan-overwrite");
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).expect("failed to create cwd");
        let config = test_user_config();
        let project = ProjectsDir::new(&root)
            .for_cwd(&cwd, &config)
            .expect("failed to create project dir");
        let settings = settings_for_cwd(&config, &cwd);
        let (event_tx, _event_rx) = mpsc::channel(16);
        let (_request_tx, request_rx) = mpsc::channel(1);
        let mut runtime = AgentRuntime::new(
            event_tx,
            test_persistence_tx(),
            request_rx,
            settings,
            project.clone(),
        );

        runtime.set_active_profile(ActiveProfile::Plan);
        runtime.messages = vec![Message::new(
            Role::Assistant,
            vec![ContentBlock::from_text(
                "<proposed_plan>\n# First Plan\n\n- Earlier.\n</proposed_plan>".to_string(),
            )],
        )];
        let first = runtime
            .persist_latest_proposed_plan()
            .await
            .expect("persist first proposed plan should succeed")
            .expect("first plan should be extracted");

        runtime.messages = vec![Message::new(
            Role::Assistant,
            vec![ContentBlock::from_text(
                "<proposed_plan>\n# Second Plan\n\n- Later.\n</proposed_plan>".to_string(),
            )],
        )];
        let second = runtime
            .persist_latest_proposed_plan()
            .await
            .expect("persist second proposed plan should succeed")
            .expect("second plan should be extracted");

        let expected_path = project.path().join("plans").join("plan.md");
        assert_eq!(first.id, "plan");
        assert_eq!(second.id, "plan");
        assert_eq!(first.path, expected_path);
        assert_eq!(second.path, expected_path);
        assert_eq!(
            std::fs::read_to_string(&second.path).expect("plan file should exist"),
            "# Second Plan\n\n- Later."
        );

        let entries = std::fs::read_dir(project.path().join("plans"))
            .expect("plans dir should exist")
            .collect::<Result<Vec<_>, _>>()
            .expect("plans dir should be readable");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path(), expected_path);
    }

    #[tokio::test]
    async fn proposed_plan_persistence_ignores_inline_tag_reference() {
        ensure_test_persistence().await;

        let root = unique_temp_root("proposed-plan-inline-reference");
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).expect("failed to create cwd");
        let config = test_user_config();
        let project = ProjectsDir::new(&root)
            .for_cwd(&cwd, &config)
            .expect("failed to create project dir");
        let settings = settings_for_cwd(&config, &cwd);
        let (event_tx, _event_rx) = mpsc::channel(16);
        let (_request_tx, request_rx) = mpsc::channel(1);
        let mut runtime = AgentRuntime::new(
            event_tx,
            test_persistence_tx(),
            request_rx,
            settings,
            project.clone(),
        );

        runtime.set_active_profile(ActiveProfile::Plan);
        runtime.messages = vec![Message::new(
            Role::Assistant,
            vec![ContentBlock::from_text(
                concat!(
                    "好的，让我把完整计划整理成规范的 `<proposed_plan>` 块。\n\n",
                    "<proposed_plan>\n",
                    "# 添加 `/thinking` 命令\n\n",
                    "## 摘要\n\n",
                    "- 切换思考块展示。\n",
                    "</proposed_plan>",
                )
                .to_string(),
            )],
        )];

        let plan = runtime
            .persist_latest_proposed_plan()
            .await
            .expect("persist proposed plan should succeed")
            .expect("plan should be extracted");

        assert_eq!(plan.title, "添加 `/thinking` 命令");
        assert!(plan.markdown.starts_with("# 添加 `/thinking` 命令"));
        assert!(!plan.markdown.starts_with("` 块。"));
        assert!(!plan.markdown.contains("<proposed_plan>"));
    }

    #[tokio::test]
    async fn proposed_plan_is_forwarded_as_plan_persistence_event() {
        ensure_test_persistence().await;

        let root = unique_temp_root("proposed-plan-db");
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).expect("failed to create cwd");
        let config = test_user_config();
        let project = ProjectsDir::new(&root)
            .for_cwd(&cwd, &config)
            .expect("failed to create project dir");
        let settings = settings_for_cwd(&config, &cwd);
        let (event_tx, _event_rx) = mpsc::channel(16);
        let (persistence_tx, mut persistence_rx) = test_persistence_channel();
        let (_request_tx, request_rx) = mpsc::channel(1);
        let mut runtime = AgentRuntime::new(
            event_tx,
            persistence_tx,
            request_rx,
            settings,
            project.clone(),
        );

        runtime.messages = vec![Message::from_user_text("seed".to_string())];
        runtime
            .create_session(Some(HistoryItem::Message(runtime.messages[0].clone())))
            .await;
        let session_id = runtime.session_id.clone().expect("session id should exist");
        runtime.set_active_profile(ActiveProfile::Plan);
        runtime.messages = vec![Message::new(
            Role::Assistant,
            vec![ContentBlock::from_text(
                "<proposed_plan>\n# DB Plan\n\n- Keep style.\n</proposed_plan>".to_string(),
            )],
        )];

        let plan = runtime
            .persist_latest_proposed_plan()
            .await
            .expect("persist proposed plan should succeed")
            .expect("plan should be extracted");

        let mut saw_plan_event = false;
        while let Ok(event) = persistence_rx.try_recv() {
            if let RuntimePersistenceEvent::InsertPlanMessage {
                session_id: event_session_id,
                plan: event_plan,
            } = event
            {
                assert_eq!(event_session_id, session_id);
                assert_eq!(event_plan.markdown, plan.markdown);
                saw_plan_event = true;
            }
        }
        assert!(saw_plan_event);
    }

    #[tokio::test]
    async fn compact_summary_is_forwarded_as_special_persistence_event() {
        ensure_test_persistence().await;

        let root = unique_temp_root("compact-summary-db");
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).expect("failed to create cwd");
        let config = test_user_config();
        let project = ProjectsDir::new(&root)
            .for_cwd(&cwd, &config)
            .expect("failed to create project dir");
        let settings = settings_for_cwd(&config, &cwd);
        let (event_tx, _event_rx) = mpsc::channel(16);
        let (persistence_tx, mut persistence_rx) = test_persistence_channel();
        let (_request_tx, request_rx) = mpsc::channel(1);
        let mut runtime = AgentRuntime::new(
            event_tx,
            persistence_tx.clone(),
            request_rx,
            settings,
            project,
        );

        runtime.messages = vec![Message::from_user_text("seed".to_string())];
        runtime
            .create_session(Some(HistoryItem::Message(runtime.messages[0].clone())))
            .await;
        let session_id = runtime.session_id.clone().expect("session id should exist");
        let event = crate::types::events::CompactSummaryFinishedEvent {
            trigger: crate::types::events::CompactTrigger::Manual,
            summary: "# Summary\n\n- Keep this.".to_string(),
            after_tokens: 42,
            session_id: Some(session_id.clone()),
            agent_label: None,
        };

        persist_compact_summary_event(&session_id, &event, &persistence_tx).await;

        let mut saw_summary_event = false;
        while let Ok(event_out) = persistence_rx.try_recv() {
            if let RuntimePersistenceEvent::InsertCompactSummaryMessage {
                session_id: event_session_id,
                summary,
            } = event_out
            {
                assert_eq!(event_session_id, session_id);
                assert_eq!(summary.markdown, event.summary);
                saw_summary_event = true;
            }
        }
        assert!(saw_summary_event);
    }

    #[tokio::test]
    async fn manual_compact_noop_emits_terminal_warning() {
        ensure_test_persistence().await;

        let root = unique_temp_root("compact-noop-warning");
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).expect("failed to create cwd");
        let config = test_user_config();
        let project = ProjectsDir::new(&root)
            .for_cwd(&cwd, &config)
            .expect("failed to create project dir");
        let settings = settings_for_cwd(&config, &cwd);
        let (event_tx, mut event_rx) = mpsc::channel(16);
        let (_request_tx, request_rx) = mpsc::channel(1);
        let mut runtime = AgentRuntime::new(
            event_tx,
            test_persistence_tx(),
            request_rx,
            settings,
            project,
        );

        runtime.messages = vec![Message::from_user_text("seed".to_string())];
        runtime
            .create_session(Some(HistoryItem::Message(runtime.messages[0].clone())))
            .await;
        let _ = drain_events(&mut event_rx);

        runtime
            .force_compact_current_session(None)
            .await
            .expect("noop compact should succeed");

        let events = drain_events(&mut event_rx);
        assert!(events.iter().any(|event| {
            matches!(
                event,
                RuntimeToUiEvent::Notification(notification)
                    if notification.kind == NotificationKind::Warn
                        && notification.message.contains("还不需要压缩")
            )
        }));
    }

    #[tokio::test]
    async fn approve_plan_adds_short_user_confirmation_only() {
        ensure_test_persistence().await;

        let root = unique_temp_root("approve-plan-short-message");
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).expect("failed to create cwd");
        let config = test_user_config();
        let project = ProjectsDir::new(&root)
            .for_cwd(&cwd, &config)
            .expect("failed to create project dir");
        let mut settings = settings_for_cwd(&config, &cwd);
        settings.max_turns = Some(0);
        let (event_tx, mut event_rx) = mpsc::channel(16);
        let (_request_tx, request_rx) = mpsc::channel(1);
        let mut runtime = AgentRuntime::new(
            event_tx,
            test_persistence_tx(),
            request_rx,
            settings,
            project.clone(),
        );

        runtime.messages = vec![Message::new(
            Role::Assistant,
            vec![ContentBlock::from_text(
                "<proposed_plan>\n# Approved plan\n\n- Execute it.\n</proposed_plan>".to_string(),
            )],
        )];
        runtime
            .create_session(Some(HistoryItem::Message(runtime.messages[0].clone())))
            .await;

        runtime
            .resolve_plan_approval(
                "unused-plan-id",
                PlanApprovalAction::Approve {
                    profile: PlanExecutionProfile::Main,
                },
            )
            .await;

        let approval = runtime
            .messages
            .iter()
            .rev()
            .find(|message| message.role == Role::User)
            .expect("approval message should be added");
        assert_eq!(
            text_content(approval),
            "Approved. Implement the proposed plan now."
        );
        assert!(!text_content(approval).contains("# Approved plan"));

        let mut saw_short_approval_event = false;
        while let Ok(event) = event_rx.try_recv() {
            if let RuntimeToUiEvent::UserMessageInjected(HistoryItem::Message(message)) = event
                && text_content(&message) == "Approved. Implement the proposed plan now."
            {
                saw_short_approval_event = true;
            }
        }
        assert!(saw_short_approval_event);
    }

    #[tokio::test]
    async fn approve_plan_can_start_in_auto_profile() {
        ensure_test_persistence().await;

        let root = unique_temp_root("approve-plan-auto");
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).expect("failed to create cwd");
        let config = test_user_config();
        let project = ProjectsDir::new(&root)
            .for_cwd(&cwd, &config)
            .expect("failed to create project dir");
        let mut settings = settings_for_cwd(&config, &cwd);
        settings.max_turns = Some(0);
        let (event_tx, _event_rx) = mpsc::channel(16);
        let (_request_tx, request_rx) = mpsc::channel(1);
        let mut runtime = AgentRuntime::new(
            event_tx,
            test_persistence_tx(),
            request_rx,
            settings,
            project,
        );

        runtime.messages = vec![Message::new(
            Role::Assistant,
            vec![ContentBlock::from_text(
                "<proposed_plan>\n# Approved plan\n\n- Execute it.\n</proposed_plan>".to_string(),
            )],
        )];
        runtime
            .create_session(Some(HistoryItem::Message(runtime.messages[0].clone())))
            .await;

        runtime
            .resolve_plan_approval(
                "unused-plan-id",
                PlanApprovalAction::Approve {
                    profile: PlanExecutionProfile::Auto,
                },
            )
            .await;

        assert_eq!(runtime.active_profile(), ActiveProfile::Auto);
    }

    #[tokio::test]
    async fn resolve_plan_approval_broadcasts_resolved_event() {
        ensure_test_persistence().await;

        let root = unique_temp_root("resolve-plan-broadcast");
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).expect("failed to create cwd");
        let config = test_user_config();
        let project = ProjectsDir::new(&root)
            .for_cwd(&cwd, &config)
            .expect("failed to create project dir");
        let settings = settings_for_cwd(&config, &cwd);
        let (event_tx, mut event_rx) = mpsc::channel(16);
        let (_request_tx, request_rx) = mpsc::channel(1);
        let mut runtime = AgentRuntime::new(
            event_tx,
            test_persistence_tx(),
            request_rx,
            settings,
            project,
        );

        runtime
            .resolve_plan_approval("plan_1", PlanApprovalAction::ContinueDiscussing)
            .await;

        let mut saw_resolved = false;
        while let Ok(event) = event_rx.try_recv() {
            if let RuntimeToUiEvent::PlanApprovalResolved { plan_id, action } = event {
                assert_eq!(plan_id, "plan_1");
                assert_eq!(action, PlanApprovalAction::ContinueDiscussing);
                saw_resolved = true;
            }
        }
        assert!(saw_resolved);
    }

    #[tokio::test]
    async fn approve_and_compact_creates_new_session_with_plan_as_initial_user_message() {
        ensure_test_persistence().await;

        let root = unique_temp_root("approve-compact");
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).expect("failed to create cwd");
        let config = test_user_config();
        let project = ProjectsDir::new(&root)
            .for_cwd(&cwd, &config)
            .expect("failed to create project dir");
        let mut settings = settings_for_cwd(&config, &cwd);
        settings.max_turns = Some(0);
        let (event_tx, mut event_rx) = mpsc::channel(16);
        let (_request_tx, request_rx) = mpsc::channel(1);
        let mut runtime = AgentRuntime::new(
            event_tx,
            test_persistence_tx(),
            request_rx,
            settings,
            project.clone(),
        );

        runtime.messages = vec![Message::from_user_text("old conversation".to_string())];
        runtime
            .create_session(Some(HistoryItem::Message(runtime.messages[0].clone())))
            .await;
        let old_session_id = runtime.session_id.clone();

        let plan_id = "plan";
        let plans_dir = project.path().join("plans");
        std::fs::create_dir_all(&plans_dir).expect("failed to create plans dir");
        std::fs::write(
            plans_dir.join("plan.md"),
            "# Approved plan\n\n1. Execute it.",
        )
        .expect("failed to write plan");

        runtime
            .resolve_plan_approval(
                plan_id,
                PlanApprovalAction::ApproveAndCompact {
                    profile: PlanExecutionProfile::Main,
                },
            )
            .await;

        let new_session_id = runtime.session_id.clone();
        assert_ne!(new_session_id, old_session_id);
        assert_eq!(runtime.active_profile(), ActiveProfile::Main);
        assert_eq!(runtime.messages.len(), 1);
        assert_eq!(runtime.messages[0].role, Role::User);
        assert!(
            text_content(&runtime.messages[0]).contains("Implement the plan in a fresh context")
        );
        assert!(text_content(&runtime.messages[0]).contains("re-read files as needed"));
        assert!(text_content(&runtime.messages[0]).contains("Approved plan:"));
        assert!(text_content(&runtime.messages[0]).contains("# Approved plan"));

        let session_dir = runtime
            .session_dir
            .as_ref()
            .expect("new session dir should exist");
        assert_eq!(
            session_dir
                .load_history()
                .expect("failed to load persisted history"),
            runtime.messages
        );

        let mut saw_new_session_event = false;
        while let Ok(event) = event_rx.try_recv() {
            if let RuntimeToUiEvent::SessionChanged {
                session_id: Some(session_id),
                messages,
                ..
            } = event
            {
                if Some(session_id) == new_session_id {
                    assert_eq!(messages.len(), 1);
                    saw_new_session_event = true;
                }
            }
        }
        assert!(saw_new_session_event);
    }

    #[tokio::test]
    async fn approve_and_compact_can_start_new_session_in_auto_profile() {
        ensure_test_persistence().await;

        let root = unique_temp_root("approve-compact-auto");
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).expect("failed to create cwd");
        let config = test_user_config();
        let project = ProjectsDir::new(&root)
            .for_cwd(&cwd, &config)
            .expect("failed to create project dir");
        let mut settings = settings_for_cwd(&config, &cwd);
        settings.max_turns = Some(0);
        let (event_tx, _event_rx) = mpsc::channel(16);
        let (_request_tx, request_rx) = mpsc::channel(1);
        let mut runtime = AgentRuntime::new(
            event_tx,
            test_persistence_tx(),
            request_rx,
            settings,
            project.clone(),
        );

        runtime.messages = vec![Message::from_user_text("old conversation".to_string())];
        runtime
            .create_session(Some(HistoryItem::Message(runtime.messages[0].clone())))
            .await;
        let old_session_id = runtime.session_id.clone();

        let plans_dir = project.path().join("plans");
        std::fs::create_dir_all(&plans_dir).expect("failed to create plans dir");
        std::fs::write(
            plans_dir.join("plan.md"),
            "# Approved plan\n\n1. Execute it.",
        )
        .expect("failed to write plan");

        runtime
            .resolve_plan_approval(
                "plan",
                PlanApprovalAction::ApproveAndCompact {
                    profile: PlanExecutionProfile::Auto,
                },
            )
            .await;

        assert_ne!(runtime.session_id, old_session_id);
        assert_eq!(runtime.active_profile(), ActiveProfile::Auto);
        assert_eq!(runtime.messages.len(), 1);
        assert!(text_content(&runtime.messages[0]).contains("Approved plan:"));
    }

    #[tokio::test]
    async fn switch_session_resets_active_profile_to_main_and_notifies_ui() {
        ensure_test_persistence().await;

        let root = unique_temp_root("switch-session-mode");
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).expect("failed to create cwd");
        let config = test_user_config();
        let project = ProjectsDir::new(&root)
            .for_cwd(&cwd, &config)
            .expect("failed to create project dir");
        let settings = settings_for_cwd(&config, &cwd);
        let (event_tx, mut event_rx) = mpsc::channel(16);
        let (_request_tx, request_rx) = mpsc::channel(1);
        let mut runtime = AgentRuntime::new(
            event_tx,
            test_persistence_tx(),
            request_rx,
            settings,
            project,
        );

        runtime.messages = vec![Message::from_user_text("session body".to_string())];
        runtime
            .create_session(Some(HistoryItem::Message(runtime.messages[0].clone())))
            .await;
        let session_id = runtime.session_id.clone().expect("session id should exist");
        while event_rx.try_recv().is_ok() {}

        runtime.set_active_profile(ActiveProfile::Plan);
        runtime
            .switch_session(LoadedSession {
                session_id: session_id.clone(),
                provider: runtime.settings.active_provider.clone(),
                model: runtime.settings.model.clone(),
                thinking_effort: runtime.settings.thinking_effort,
                active_profile: ActiveProfile::Main,
                title: Some("session body".to_string()),
                messages: vec![HistoryItem::Message(runtime.messages[0].clone())],
                subagents: Vec::new(),
                usage: SessionUsageSnapshot::default(),
            })
            .await;

        assert_eq!(runtime.active_profile(), ActiveProfile::Main);
        assert!(
            runtime
                .settings
                .system_prompt
                .as_deref()
                .expect("system prompt should be rebuilt")
                .contains("<core_behavior>")
        );
        assert!(
            !runtime
                .settings
                .system_prompt
                .as_deref()
                .expect("system prompt should be rebuilt")
                .contains("<plan_mode_instructions>")
        );

        let mut saw_main_mode_event = false;
        while let Ok(event) = event_rx.try_recv() {
            if matches!(
                event,
                RuntimeToUiEvent::ActiveProfileChanged(ActiveProfile::Main)
            ) {
                saw_main_mode_event = true;
            }
        }
        assert!(saw_main_mode_event);
    }

    #[tokio::test]
    async fn event_processor_auto_profile_resolves_permission_pause_without_ui() {
        ensure_test_persistence().await;

        let root = unique_temp_root("auto-profile-pause-runtime");
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).expect("failed to create cwd");
        let config = test_user_config();
        let project = ProjectsDir::new(&root)
            .for_cwd(&cwd, &config)
            .expect("failed to create project dir");
        let settings = settings_for_cwd(&config, &cwd);
        let (event_tx, mut event_rx) = mpsc::channel(16);
        let (_request_tx, request_rx) = mpsc::channel(1);
        let mut runtime = AgentRuntime::new(
            event_tx,
            test_persistence_tx(),
            request_rx,
            settings,
            project,
        );
        runtime.create_session(None).await;
        runtime.set_active_profile(ActiveProfile::Auto);
        drain_events(&mut event_rx);

        let (engine_tx, engine_rx) = mpsc::channel(4);
        let active_profile_handle = Arc::clone(&runtime.active_profile);
        let (tool_pause_resolver, permission_rx) = permission_tool_pause_resolver("tool_1");
        let processor = runtime
            .spawn_event_processor(
                engine_rx,
                ActiveProfile::Main,
                active_profile_handle,
                tool_pause_resolver,
            )
            .await;

        engine_tx
            .send(EngineToRuntimeEvent::ToolPauseRequested(permission_pause(
                "tool_1",
            )))
            .await
            .expect("pause event should send");

        let response = tokio::time::timeout(Duration::from_secs(1), permission_rx)
            .await
            .expect("auto permission response should arrive")
            .expect("auto permission waiter should stay open");
        assert_eq!(
            response,
            ToolPauseResponse::Permission {
                approved: true,
                note: None,
            }
        );
        assert!(
            !drain_events(&mut event_rx)
                .into_iter()
                .any(|event| matches!(event, RuntimeToUiEvent::ToolPauseRequested(_)))
        );

        drop(engine_tx);
        processor.await.expect("processor should finish");
    }

    #[tokio::test]
    async fn event_processor_main_profile_forwards_permission_pause_to_ui() {
        ensure_test_persistence().await;

        let root = unique_temp_root("main-profile-pause-ui");
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).expect("failed to create cwd");
        let config = test_user_config();
        let project = ProjectsDir::new(&root)
            .for_cwd(&cwd, &config)
            .expect("failed to create project dir");
        let settings = settings_for_cwd(&config, &cwd);
        let (event_tx, mut event_rx) = mpsc::channel(16);
        let (_request_tx, request_rx) = mpsc::channel(1);
        let mut runtime = AgentRuntime::new(
            event_tx,
            test_persistence_tx(),
            request_rx,
            settings,
            project,
        );
        runtime.create_session(None).await;
        drain_events(&mut event_rx);

        let (engine_tx, engine_rx) = mpsc::channel(4);
        let active_profile_handle = Arc::clone(&runtime.active_profile);
        let processor = runtime
            .spawn_event_processor(
                engine_rx,
                ActiveProfile::Main,
                active_profile_handle,
                empty_tool_pause_resolver(),
            )
            .await;

        engine_tx
            .send(EngineToRuntimeEvent::ToolPauseRequested(permission_pause(
                "tool_1",
            )))
            .await
            .expect("pause event should send");

        let event = tokio::time::timeout(Duration::from_secs(1), event_rx.recv())
            .await
            .expect("ui pause event should arrive")
            .expect("ui event channel should stay open");
        let RuntimeToUiEvent::ToolPauseRequested(req) = event else {
            panic!("expected tool pause event");
        };
        assert_eq!(req.tool_use_id, "tool_1");

        drop(engine_tx);
        processor.await.expect("processor should finish");
    }

    #[tokio::test]
    async fn event_processor_forwards_produced_user_message_to_ui() {
        ensure_test_persistence().await;

        let root = unique_temp_root("produced-user-message-ui");
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).expect("failed to create cwd");
        let config = test_user_config();
        let project = ProjectsDir::new(&root)
            .for_cwd(&cwd, &config)
            .expect("failed to create project dir");
        let settings = settings_for_cwd(&config, &cwd);
        let (event_tx, mut event_rx) = mpsc::channel(16);
        let (persistence_tx, mut persistence_rx) = test_persistence_channel();
        let (_request_tx, request_rx) = mpsc::channel(1);
        let mut runtime =
            AgentRuntime::new(event_tx, persistence_tx, request_rx, settings, project);
        runtime.create_session(None).await;
        let session_id = runtime.session_id.clone().expect("session id should exist");
        drain_events(&mut event_rx);

        let (engine_tx, engine_rx) = mpsc::channel(4);
        let active_profile_handle = Arc::clone(&runtime.active_profile);
        let processor = runtime
            .spawn_event_processor(
                engine_rx,
                ActiveProfile::Main,
                active_profile_handle,
                empty_tool_pause_resolver(),
            )
            .await;
        let message = Message::from_user_text("intervention".to_string());

        engine_tx
            .send(EngineToRuntimeEvent::UserMessageProduced(message.clone()))
            .await
            .expect("user message event should send");

        let event = tokio::time::timeout(Duration::from_secs(1), event_rx.recv())
            .await
            .expect("ui user message event should arrive")
            .expect("ui event channel should stay open");
        let RuntimeToUiEvent::UserMessageInjected(HistoryItem::Message(event_message)) = event
        else {
            panic!("expected user message injection");
        };
        assert_eq!(event_message, message);

        let mut saw_persistence_event = false;
        while let Ok(event) = persistence_rx.try_recv() {
            if let RuntimePersistenceEvent::InsertMessage {
                session_id: event_session_id,
                role,
                blocks,
                ..
            } = event
                && event_session_id == session_id
                && role == "user"
                && blocks == message.content
            {
                saw_persistence_event = true;
            }
        }
        assert!(saw_persistence_event);

        drop(engine_tx);
        processor.await.expect("processor should finish");
    }

    #[tokio::test]
    async fn split_tool_result_history_writes_image_only_to_jsonl() {
        ensure_test_persistence().await;

        let root = unique_temp_root("split-tool-result-history");
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).expect("failed to create cwd");
        let config = test_user_config();
        let project = ProjectsDir::new(&root)
            .for_cwd(&cwd, &config)
            .expect("failed to create project dir");
        let settings = settings_for_cwd(&config, &cwd);
        let (event_tx, _event_rx) = mpsc::channel(16);
        let (persistence_tx, mut persistence_rx) = test_persistence_channel();
        let (_request_tx, request_rx) = mpsc::channel(1);
        let mut runtime =
            AgentRuntime::new(event_tx, persistence_tx, request_rx, settings, project);
        runtime.create_session(None).await;
        let session_id = runtime.session_id.clone().expect("session id should exist");
        let session_dir = runtime
            .session_dir
            .clone()
            .expect("session dir should exist");

        let (engine_tx, engine_rx) = mpsc::channel(4);
        let active_profile_handle = Arc::clone(&runtime.active_profile);
        let processor = runtime
            .spawn_event_processor(
                engine_rx,
                ActiveProfile::Main,
                active_profile_handle,
                empty_tool_pause_resolver(),
            )
            .await;

        let display_msg = Message::new(
            Role::User,
            vec![ContentBlock::from_tool_result(
                "toolu_image".to_string(),
                false,
                "Loaded image".to_string(),
            )],
        );
        let llm_msg = Message::new(
            Role::User,
            vec![
                ContentBlock::from_tool_result(
                    "toolu_image".to_string(),
                    false,
                    "Loaded image".to_string(),
                ),
                ContentBlock::from_base64_image("image/png".to_string(), "abc123".to_string()),
            ],
        );

        engine_tx
            .send(EngineToRuntimeEvent::LlmHistoryProduced(llm_msg.clone()))
            .await
            .expect("llm history event should send");
        engine_tx
            .send(EngineToRuntimeEvent::ToolResultsDisplayProduced(
                display_msg.clone(),
            ))
            .await
            .expect("display history event should send");
        drop(engine_tx);
        processor.await.expect("processor should finish");

        assert_eq!(session_dir.load_history().unwrap(), vec![llm_msg]);

        let mut saw_display_message = false;
        while let Ok(event) = persistence_rx.try_recv() {
            if let RuntimePersistenceEvent::InsertMessage {
                session_id: event_session_id,
                blocks,
                ..
            } = event
                && event_session_id == session_id
                && blocks == display_msg.content
            {
                assert!(
                    !blocks
                        .iter()
                        .any(|block| matches!(block, ContentBlock::Image(_)))
                );
                saw_display_message = true;
            }
        }
        assert!(saw_display_message);
    }

    #[tokio::test]
    async fn usage_events_update_main_and_subagent_session_totals() {
        ensure_test_persistence().await;

        let root = unique_temp_root("usage-events");
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).expect("failed to create cwd");
        let config = test_user_config();
        let project = ProjectsDir::new(&root)
            .for_cwd(&cwd, &config)
            .expect("failed to create project dir");
        let settings = settings_for_cwd(&config, &cwd);
        let (event_tx, mut event_rx) = mpsc::channel(16);
        let (persistence_tx, mut persistence_rx) = test_persistence_channel();
        let (_request_tx, request_rx) = mpsc::channel(1);
        let mut runtime =
            AgentRuntime::new(event_tx, persistence_tx, request_rx, settings, project);

        runtime.messages = vec![Message::from_user_text("session body".to_string())];
        runtime
            .create_session(Some(HistoryItem::Message(runtime.messages[0].clone())))
            .await;
        let parent_session_id = runtime.session_id.clone().expect("session id should exist");
        drain_events(&mut event_rx);
        while persistence_rx.try_recv().is_ok() {}

        let subagent_session_id = Uuid::new_v4().to_string();

        let (engine_tx, engine_rx) = mpsc::channel(4);
        let active_profile_handle = Arc::clone(&runtime.active_profile);
        let processor = runtime
            .spawn_event_processor(
                engine_rx,
                ActiveProfile::Main,
                active_profile_handle,
                empty_tool_pause_resolver(),
            )
            .await;

        engine_tx
            .send(EngineToRuntimeEvent::UsageRecorded(
                crate::types::usage::Usage {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                    cached_tokens: 3,
                },
            ))
            .await
            .expect("usage event should send");
        engine_tx
            .send(EngineToRuntimeEvent::SubagentUsageRecorded {
                session_id: subagent_session_id.clone(),
                usage: crate::types::usage::Usage {
                    prompt_tokens: 7,
                    completion_tokens: 8,
                    cached_tokens: 4,
                },
            })
            .await
            .expect("subagent usage event should send");
        drop(engine_tx);
        processor.await.expect("processor should finish");

        let mut saw_parent_usage = false;
        let mut saw_subagent_usage = false;
        let mut saw_parent_subagent_usage = false;
        while let Ok(event) = persistence_rx.try_recv() {
            match event {
                RuntimePersistenceEvent::RecordSessionUsage { session_id, usage }
                    if session_id == parent_session_id =>
                {
                    assert_eq!(usage.total_tokens(), 15);
                    assert_eq!(usage.cached_tokens, 3);
                    saw_parent_usage = true;
                }
                RuntimePersistenceEvent::RecordSessionUsage { session_id, usage }
                    if session_id == subagent_session_id =>
                {
                    assert_eq!(usage.total_tokens(), 15);
                    assert_eq!(usage.cached_tokens, 4);
                    saw_subagent_usage = true;
                }
                RuntimePersistenceEvent::RecordParentSubagentUsage { session_id, usage }
                    if session_id == parent_session_id =>
                {
                    assert_eq!(usage.total_tokens(), 15);
                    assert_eq!(usage.cached_tokens, 4);
                    saw_parent_subagent_usage = true;
                }
                _ => {}
            }
        }
        assert!(saw_parent_usage);
        assert!(saw_subagent_usage);
        assert!(saw_parent_subagent_usage);

        let mut last_usage = None;
        while let Ok(event) = event_rx.try_recv() {
            if let RuntimeToUiEvent::UsageChanged(snapshot) = event {
                last_usage = Some(snapshot);
            }
        }
        let snapshot = last_usage.expect("usage snapshot should be emitted");
        assert_eq!(snapshot.current_context_tokens, 15);
        assert_eq!(snapshot.total_tokens, 30);
        assert_eq!(snapshot.total_cached_tokens, 7);
    }
}
