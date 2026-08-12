use super::capabilities::CapabilityStore;
use crate::engine::QueryEngine;
use crate::mcp::McpManager;
use crate::subagents::{AgentTaskCompletion, AgentTaskSupervisor};
use crate::tools::ToolRegistry;
use omini_config::Settings;
use omini_config::project::{ProjectDir, ThreadDir};
use omini_domain::display::DisplayMessage;
use omini_domain::events::{ActiveProfile, ThreadUsageSnapshot};
use omini_domain::message::Message;
use omini_permissions::PermissionEngine;
use omini_provider_api::LlmClient;
use omini_runtime_contract::persistence::RuntimePersistenceEvent;
use omini_runtime_contract::{RuntimeToServerEvent, ServerToRuntimeEvent};
use std::sync::atomic::{AtomicBool, AtomicI64};
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::mpsc;

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

pub struct AgentRuntimeChannels {
    pub event_tx: mpsc::Sender<RuntimeToServerEvent>,
    pub persistence_tx: mpsc::Sender<RuntimePersistenceEvent>,
    pub request_rx: mpsc::Receiver<ServerToRuntimeEvent>,
}

pub struct AgentRuntimeDeps {
    pub settings: Settings,
    pub project: ProjectDir,
    pub thread_id: String,
    pub thread_dir: ThreadDir,
    pub messages: Vec<Message>,
    pub llm_context_version: i64,
    pub usage: ThreadUsageSnapshot,
    pub active_profile: ActiveProfile,
    pub agent_tasks: Vec<omini_domain::events::AgentTaskInfo>,
}

#[derive(Debug)]
pub(super) enum RunStart {
    /// 启动前将最新 runtime 消息同时写入 LLM 历史和 UI 历史。
    UserMessage,
    /// 启动前将最新 runtime 消息写入 LLM 上下文，将 UI-only display 消息写入 UI 历史。
    SplitDisplayMessage { display_message: DisplayMessage },
    /// 由待持久化的 Agent task completion 启动；落库前禁止请求 provider。
    PendingAgentTaskNotification,
    /// 通知已在上一个 query 的终止边界持久化，只需继续请求 provider。
    PersistedAgentTaskNotification,
}

impl RunStart {
    pub(super) fn kind(&self) -> &'static str {
        match self {
            Self::UserMessage => "user_message",
            Self::SplitDisplayMessage { .. } => "split_display_message",
            Self::PendingAgentTaskNotification => "pending_agent_task_notification",
            Self::PersistedAgentTaskNotification => "persisted_agent_task_notification",
        }
    }
}

/// Agent 运行时。
///
/// 维护自己的对话历史，通过 channel 与 server/facade 双向通信。
/// 一次 `ServerToRuntimeEvent::SendMessage` 可能触发多轮 LLM 调用和工具执行，
/// 直到 LLM 自然结束或达到最大轮次。
///
/// 自身不负责创建 thread；调用方必须传入 server 已经持久化的 thread 与目录句柄。
pub struct AgentRuntime {
    pub(crate) thread_id: String,
    pub(crate) thread_dir: ThreadDir,
    /// 向 server/facade 发送 runtime 事件。
    pub(super) event_tx: mpsc::Sender<RuntimeToServerEvent>,
    /// 向外部 server 转发展示/SQLite 级持久化事件。
    pub(super) persistence_tx: mpsc::Sender<RuntimePersistenceEvent>,
    /// 接收 server/facade 投递的 runtime 命令。
    pub(super) request_rx: mpsc::Receiver<ServerToRuntimeEvent>,
    /// 运行时配置。
    pub(crate) settings: Settings,
    /// 当前项目目录。
    pub(crate) project: ProjectDir,
    /// 运行时自主维护的对话历史。
    pub(crate) messages: Vec<Message>,
    pub(crate) llm_context_version: Arc<AtomicI64>,
    /// LLM 客户端。
    pub(super) llm_client: LlmClient,
    /// 查询引擎。
    pub(super) query_engine: QueryEngine,
    /// 工具注册表，持有所有注册的工具。
    pub(super) tool_registry: Arc<ToolRegistry>,
    /// 从有效配置加载的 MCP 服务管理器。
    pub(super) mcp_manager: Arc<McpManager>,
    /// runtime 是否已在 query 前等待过 MCP 启动。
    pub(super) mcp_initialized: bool,
    /// 长期存活的后台 task supervisor，不依赖前台运行生命周期。
    pub(super) task_supervisor: Arc<AgentTaskSupervisor>,
    pub(super) task_completion_rx: mpsc::UnboundedReceiver<AgentTaskCompletion>,
    /// runtime 管理的能力注册状态；每次 query 开始时生成只读快照。
    pub(super) capabilities: Arc<CapabilityStore>,
    /// 取消标志，用于 CancelRun。
    pub(super) cancelled: Arc<AtomicBool>,
    /// 当前活跃 profile，供 runtime 主循环和运行中事件处理器共享读取。
    pub(crate) active_profile: Arc<RwLock<ActiveProfile>>,
    /// 当前 thread 的 usage 快照；SQLite 落库由 server 处理。
    pub(super) thread_usage: Arc<Mutex<ThreadUsageSnapshot>>,
}

impl AgentRuntime {
    pub fn new(channels: AgentRuntimeChannels, mut deps: AgentRuntimeDeps) -> Self {
        deps.active_profile = ActiveProfile::Main;
        Self::new_with_active_profile(channels, deps)
    }

    pub fn new_with_active_profile(channels: AgentRuntimeChannels, deps: AgentRuntimeDeps) -> Self {
        let handles = RuntimeCapabilityHandles::load(&deps.settings);
        Self::with_capability_handles(channels, deps, handles)
    }

    pub(crate) fn with_capability_handles(
        channels: AgentRuntimeChannels,
        deps: AgentRuntimeDeps,
        handles: RuntimeCapabilityHandles,
    ) -> Self {
        let AgentRuntimeChannels {
            event_tx,
            persistence_tx,
            request_rx,
        } = channels;
        let AgentRuntimeDeps {
            mut settings,
            project,
            thread_id,
            thread_dir,
            messages,
            llm_context_version,
            usage,
            active_profile,
            agent_tasks,
        } = deps;
        let llm_client = LlmClient::new(
            settings.endpoint,
            settings.api_key.clone(),
            settings.base_url.clone(),
        );
        let tool_registry = Arc::new(crate::tools::create_main_registry());
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
        let permission_sources = omini_config::permissions::load_permission_sources(
            &settings.cwd,
            dirs::home_dir().as_deref(),
            settings.permissions.clone(),
        );
        let permission_engine = Arc::new(PermissionEngine::from_sources(
            settings.cwd.clone(),
            dirs::home_dir(),
            permission_sources,
        ));
        let query_engine = QueryEngine::new(Arc::clone(&permission_engine));
        let active_profile = Arc::new(RwLock::new(active_profile));
        let thread_usage = Arc::new(Mutex::new(usage));
        let (task_completion_tx, task_completion_rx) = mpsc::unbounded_channel();
        let task_supervisor = AgentTaskSupervisor::new(
            event_tx.clone(),
            persistence_tx.clone(),
            task_completion_tx,
            query_engine.pending_tool_pauses(),
            query_engine.permission_engine(),
            Arc::clone(&active_profile),
            Arc::clone(&thread_usage),
            agent_tasks,
        );

        for diagnostic in &subagent_registry.diagnostics {
            let _ = event_tx.try_send(RuntimeToServerEvent::warning(format!(
                "Subagent: {}",
                diagnostic.message()
            )));
        }
        for diagnostic in &skill_registry.diagnostics {
            let _ = event_tx.try_send(RuntimeToServerEvent::warning(format!(
                "Skill: {}",
                diagnostic.message()
            )));
        }
        for diagnostic in permission_engine.diagnostics() {
            let _ = event_tx.try_send(RuntimeToServerEvent::warning(format!(
                "Permission: {diagnostic}"
            )));
        }

        Self {
            thread_id,
            thread_dir,
            event_tx,
            persistence_tx,
            request_rx,
            settings,
            project,
            messages,
            llm_context_version: Arc::new(AtomicI64::new(llm_context_version)),
            cancelled: Arc::new(AtomicBool::new(false)),
            llm_client,
            tool_registry,
            mcp_manager,
            mcp_initialized: false,
            task_supervisor,
            task_completion_rx,
            capabilities,
            query_engine,
            active_profile,
            thread_usage,
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
    use crate::engine::ToolPauseResolver;
    use crate::runtime::active_run;
    use crate::runtime::history;
    use crate::runtime::manual_compact::persist_compact_summary_event;
    use crate::types::events::EngineToRuntimeEvent;
    use omini_config::project::{ProjectsDir, ThreadDir};
    use omini_config::{ModelEntry, ModelTiers, ProviderConfig, Settings, UserConfig};
    use omini_domain::config::ProviderEndpointKind;
    use omini_domain::display::{
        AgentTaskNotification, AgentTaskNotificationItem, HistoryItem, UserDraft,
    };
    use omini_domain::events::{
        AgentTaskStatus, CompactSummaryFinishedEvent, CompactTrigger, PermissionPreview,
        PlanApprovalAction, PlanExecutionProfile, ToolPauseKind, ToolPauseRequest,
        ToolPauseResponse,
    };
    use omini_domain::message::{ContentBlock, Role};
    use omini_domain::usage::Usage;
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use uuid::Uuid;

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
                headers: None,
                body: None,
            },
        );

        let mut providers = HashMap::new();
        providers.insert(
            "openai".to_string(),
            ProviderConfig {
                name: Some("OpenAI".to_string()),
                endpoint: ProviderEndpointKind::OpenAI,
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
            model_tiers: ModelTiers::default(),
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

    fn drain_events(
        event_rx: &mut mpsc::Receiver<RuntimeToServerEvent>,
    ) -> Vec<RuntimeToServerEvent> {
        let mut events = Vec::new();
        while let Ok(event) = event_rx.try_recv() {
            events.push(event);
        }
        events
    }

    /// 这些测试会同步启动 run，但只验证 query 前的 runtime 副作用，不应访问真实 provider。
    /// `max_turns = 0` 会执行一次 finalization turn，不能再用作无请求的测试短路。
    fn cancel_next_run(runtime: &AgentRuntime) {
        runtime
            .cancelled
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// 为测试在 project 下建一个 thread，并返回其 id 与目录。
    ///
    /// 新架构下 `AgentRuntime` 必须依赖一个已存在的 thread,不再自己生成 UUID。
    /// 测试中用这个 helper 模拟 server 的预创建行为,然后把结果传给 `AgentRuntime::new`。
    fn create_test_thread(project: &ProjectDir) -> (String, ThreadDir) {
        let thread_id = Uuid::new_v4().to_string();
        let thread_dir = project
            .create_thread(&thread_id)
            .expect("test thread directory should be created");
        (thread_id, thread_dir)
    }

    /// 把 `AgentRuntime::new` 的样板参数(channel 等)收口,只暴露 `settings` / `project`。
    ///
    /// 用法示例:
    /// ```ignore
    /// let (mut runtime, mut event_rx) = runtime_for_thread(settings, project);
    /// ```
    fn runtime_for_thread(
        settings: Settings,
        project: ProjectDir,
    ) -> (AgentRuntime, mpsc::Receiver<RuntimeToServerEvent>) {
        let (thread_id, thread_dir) = create_test_thread(&project);
        let (event_tx, event_rx) = mpsc::channel(32);
        let (_request_tx, request_rx) = mpsc::channel(1);
        let channels = AgentRuntimeChannels {
            event_tx,
            persistence_tx: test_persistence_tx(),
            request_rx,
        };
        let deps = AgentRuntimeDeps {
            settings,
            project,
            thread_id,
            thread_dir,
            messages: Vec::new(),
            llm_context_version: 1,
            usage: ThreadUsageSnapshot::default(),
            active_profile: ActiveProfile::Main,
            agent_tasks: Vec::new(),
        };
        let runtime = AgentRuntime::new(channels, deps);
        (runtime, event_rx)
    }

    /// 和 `runtime_for_thread` 类似,但额外把 `persistence_rx` 暴露给测试,
    /// 适用于需要断言 `RuntimePersistenceEvent` 的场景。
    fn runtime_for_thread_with_persistence(
        settings: Settings,
        project: ProjectDir,
    ) -> (
        AgentRuntime,
        mpsc::Receiver<RuntimeToServerEvent>,
        mpsc::Receiver<RuntimePersistenceEvent>,
    ) {
        let (thread_id, thread_dir) = create_test_thread(&project);
        let (event_tx, event_rx) = mpsc::channel(32);
        let (persistence_tx, persistence_rx) = test_persistence_channel();
        let (_request_tx, request_rx) = mpsc::channel(1);
        let channels = AgentRuntimeChannels {
            event_tx,
            persistence_tx,
            request_rx,
        };
        let deps = AgentRuntimeDeps {
            settings,
            project,
            thread_id,
            thread_dir,
            messages: Vec::new(),
            llm_context_version: 1,
            usage: ThreadUsageSnapshot::default(),
            active_profile: ActiveProfile::Main,
            agent_tasks: Vec::new(),
        };
        let runtime = AgentRuntime::new(channels, deps);
        (runtime, event_rx, persistence_rx)
    }

    fn permission_pause(tool_use_id: &str) -> ToolPauseRequest {
        ToolPauseRequest {
            tool_use_id: tool_use_id.to_string(),
            preview_tool_use_id: None,
            tool_name: "bash".to_string(),
            permission_source: None,
            source_thread_id: None,
            source_agent_label: None,
            kind: ToolPauseKind::Permission(PermissionPreview::Custom {
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
            .for_storage_key("test-project", &config)
            .expect("failed to create project dir");
        let settings = settings_for_cwd(&config, &cwd);
        let (mut runtime, mut event_rx) = runtime_for_thread(settings, project);
        drain_events(&mut event_rx);
        cancel_next_run(&runtime);

        runtime
            .submit_user_message(
                UserDraft::plain("hello".to_string()),
                Some("echo-1".to_string()),
            )
            .await;

        let events = drain_events(&mut event_rx);
        let injected = events
            .iter()
            .position(|event| {
                matches!(
                    event,
                    RuntimeToServerEvent::UserMessageInjected {
                        item: HistoryItem::Message(message),
                        client_echo_id,
                    } if text_content(message) == "hello"
                        && client_echo_id.as_deref() == Some("echo-1")
                )
            })
            .expect("user message should be echoed to UI");
        let started = events
            .iter()
            .position(|event| matches!(event, RuntimeToServerEvent::RunStarted))
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
            .for_storage_key("test-project", &config)
            .expect("failed to create project dir");
        let settings = settings_for_cwd(&config, &cwd);
        let (mut runtime, mut event_rx) = runtime_for_thread(settings, project);

        assert_eq!(runtime.active_profile(), ActiveProfile::Main);

        runtime.toggle_active_profile().await;
        assert_eq!(runtime.active_profile(), ActiveProfile::Auto);

        runtime.toggle_active_profile().await;
        assert_eq!(runtime.active_profile(), ActiveProfile::Plan);

        runtime.toggle_active_profile().await;
        assert_eq!(runtime.active_profile(), ActiveProfile::Main);

        let mut profiles = Vec::new();
        while let Ok(event) = event_rx.try_recv() {
            if let RuntimeToServerEvent::ActiveProfileChanged(profile) = event {
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
            .for_storage_key("test-project", &config)
            .expect("failed to create project dir");
        let settings = settings_for_cwd(&config, &cwd);
        // 本测试需要把 event_tx 单独拿出来调 `active_run::toggle_active_profile`。
        let (thread_id, thread_dir) = create_test_thread(&project);
        let (event_tx, mut event_rx) = mpsc::channel(16);
        let (_request_tx, request_rx) = mpsc::channel(1);
        let channels = AgentRuntimeChannels {
            event_tx: event_tx.clone(),
            persistence_tx: test_persistence_tx(),
            request_rx,
        };
        let deps = AgentRuntimeDeps {
            settings,
            project,
            thread_id,
            thread_dir,
            messages: Vec::new(),
            llm_context_version: 1,
            usage: ThreadUsageSnapshot::default(),
            active_profile: ActiveProfile::Main,
            agent_tasks: Vec::new(),
        };
        let mut runtime = AgentRuntime::new(channels, deps);
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
                RuntimeToServerEvent::ActiveProfileChanged(profile) => Some(profile),
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
                RuntimeToServerEvent::ActiveProfileChanged(profile) => Some(profile),
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
            .for_storage_key("test-project", &config)
            .expect("failed to create project dir");
        let settings = settings_for_cwd(&config, &cwd);
        let (mut runtime, _event_rx) = runtime_for_thread(settings, project.clone());

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
            .for_storage_key("test-project", &config)
            .expect("failed to create project dir");
        let settings = settings_for_cwd(&config, &cwd);
        let (mut runtime, _event_rx) = runtime_for_thread(settings, project.clone());

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
            .for_storage_key("test-project", &config)
            .expect("failed to create project dir");
        let settings = settings_for_cwd(&config, &cwd);
        let (mut runtime, _event_rx) = runtime_for_thread(settings, project.clone());

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
            .for_storage_key("test-project", &config)
            .expect("failed to create project dir");
        let settings = settings_for_cwd(&config, &cwd);
        let (mut runtime, _event_rx, mut persistence_rx) =
            runtime_for_thread_with_persistence(settings, project.clone());

        runtime.messages = vec![Message::from_user_text("seed".to_string())];
        let thread_id = runtime.thread_id.clone();
        let expected_model_ref = format!(
            "{}/{}",
            runtime.settings.active_provider, runtime.settings.model
        );
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
                thread_id: event_thread_id,
                plan: event_plan,
                model_ref,
            } = event
            {
                assert_eq!(event_thread_id, thread_id);
                assert_eq!(event_plan.markdown, plan.markdown);
                assert_eq!(model_ref, expected_model_ref);
                saw_plan_event = true;
            }
        }
        assert!(saw_plan_event);
    }

    #[tokio::test]
    async fn proposed_plan_blocks_are_stripped_from_kind_normal_in_plan_mode() {
        ensure_test_persistence().await;

        let root = unique_temp_root("proposed-plan-strip-normal");
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).expect("failed to create cwd");
        let config = test_user_config();
        let project = ProjectsDir::new(&root)
            .for_storage_key("test-project", &config)
            .expect("failed to create project dir");
        let settings = settings_for_cwd(&config, &cwd);
        let (mut runtime, _event_rx, mut persistence_rx) =
            runtime_for_thread_with_persistence(settings, project);

        runtime.messages = vec![Message::from_user_text("seed".to_string())];
        let thread_id = runtime.thread_id.clone();
        runtime.set_active_profile(ActiveProfile::Plan);

        let original = Message::new(
            Role::Assistant,
            vec![ContentBlock::from_text(
                "Intro\n<proposed_plan>\n# Plan\n\n- Step one.\n</proposed_plan>\nOutro"
                    .to_string(),
            )],
        );

        history::persist_one(
            &thread_id,
            original.clone(),
            ActiveProfile::Plan,
            "test/model",
            &runtime.persistence_tx,
        )
        .await;

        let mut normal_event: Option<RuntimePersistenceEvent> = None;
        let mut saw_plan_event = false;
        let mut saw_llm_message = false;
        while let Ok(event) = persistence_rx.try_recv() {
            match event {
                RuntimePersistenceEvent::InsertMessage {
                    role,
                    model_ref,
                    kind,
                    blocks,
                    ..
                } if role == "assistant" && kind == "normal" => {
                    assert_eq!(model_ref.as_deref(), Some("test/model"));
                    assert!(
                        normal_event.is_none(),
                        "exactly one InsertMessage(kind=normal) should arrive"
                    );
                    normal_event = Some(RuntimePersistenceEvent::InsertMessage {
                        thread_id: thread_id.clone(),
                        role,
                        model_ref: Some("test/model".to_string()),
                        blocks,
                        kind,
                        created_at: chrono::Utc::now(),
                    });
                }
                RuntimePersistenceEvent::InsertPlanMessage { .. } => {
                    saw_plan_event = true;
                }
                RuntimePersistenceEvent::AppendLlmMessage { message, .. }
                    if message == original =>
                {
                    saw_llm_message = true;
                }
                _ => {}
            }
        }
        assert!(
            !saw_plan_event,
            "InsertPlanMessage must not come from persist_one"
        );
        let RuntimePersistenceEvent::InsertMessage { blocks, .. } =
            normal_event.expect("InsertMessage(kind=normal) should arrive")
        else {
            unreachable!();
        };
        let ContentBlock::Text(text_block) = blocks
            .first()
            .expect("stripped message should keep at least one text block")
        else {
            panic!("expected first block to be text");
        };
        assert_eq!(text_block.text, "Intro\nOutro");
        assert!(!text_block.text.contains("<proposed_plan>"));

        assert!(saw_llm_message);
    }

    #[tokio::test]
    async fn proposed_plan_blocks_are_preserved_in_non_plan_mode() {
        ensure_test_persistence().await;

        let root = unique_temp_root("proposed-plan-keep-main");
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).expect("failed to create cwd");
        let config = test_user_config();
        let project = ProjectsDir::new(&root)
            .for_storage_key("test-project", &config)
            .expect("failed to create project dir");
        let settings = settings_for_cwd(&config, &cwd);
        let (mut runtime, _event_rx, mut persistence_rx) =
            runtime_for_thread_with_persistence(settings, project);

        runtime.messages = vec![Message::from_user_text("seed".to_string())];
        let thread_id = runtime.thread_id.clone();

        let original = Message::new(
            Role::Assistant,
            vec![ContentBlock::from_text(
                "Intro\n<proposed_plan>\n# Plan\n\n- Step one.\n</proposed_plan>\nOutro"
                    .to_string(),
            )],
        );

        history::persist_one(
            &thread_id,
            original.clone(),
            ActiveProfile::Main,
            "test/model",
            &runtime.persistence_tx,
        )
        .await;

        let mut normal_blocks: Option<Vec<ContentBlock>> = None;
        while let Ok(event) = persistence_rx.try_recv() {
            if let RuntimePersistenceEvent::InsertMessage {
                role, kind, blocks, ..
            } = event
                && role == "assistant"
                && kind == "normal"
            {
                assert!(
                    normal_blocks.is_none(),
                    "exactly one InsertMessage(kind=normal) should arrive"
                );
                normal_blocks = Some(blocks);
            }
        }
        let blocks = normal_blocks.expect("InsertMessage(kind=normal) should arrive");
        let ContentBlock::Text(text_block) = blocks
            .first()
            .expect("non-plan message should keep at least one text block")
        else {
            panic!("expected first block to be text");
        };
        assert_eq!(
            text_block.text,
            "Intro\n<proposed_plan>\n# Plan\n\n- Step one.\n</proposed_plan>\nOutro"
        );
    }

    #[tokio::test]
    async fn compact_summary_is_forwarded_as_special_persistence_event() {
        ensure_test_persistence().await;

        let root = unique_temp_root("compact-summary-db");
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).expect("failed to create cwd");
        let config = test_user_config();
        let project = ProjectsDir::new(&root)
            .for_storage_key("test-project", &config)
            .expect("failed to create project dir");
        let settings = settings_for_cwd(&config, &cwd);
        // 测试需要同时拥有 `persistence_tx` 副本(传给 `persist_compact_summary_event`)
        // 和 `runtime` 内部的那一份,不能直接用 helper。
        let (thread_id, thread_dir) = create_test_thread(&project);
        let (event_tx, _event_rx) = mpsc::channel(16);
        let (persistence_tx, mut persistence_rx) = test_persistence_channel();
        let (_request_tx, request_rx) = mpsc::channel(1);
        let channels = AgentRuntimeChannels {
            event_tx,
            persistence_tx: persistence_tx.clone(),
            request_rx,
        };
        let deps = AgentRuntimeDeps {
            settings,
            project,
            thread_id,
            thread_dir,
            messages: Vec::new(),
            llm_context_version: 1,
            usage: ThreadUsageSnapshot::default(),
            active_profile: ActiveProfile::Main,
            agent_tasks: Vec::new(),
        };
        let mut runtime = AgentRuntime::new(channels, deps);

        runtime.messages = vec![Message::from_user_text("seed".to_string())];
        let thread_id = runtime.thread_id.clone();
        let event = CompactSummaryFinishedEvent {
            trigger: CompactTrigger::Manual,
            summary: "# Summary\n\n- Keep this.".to_string(),
            after_tokens: 42,
            thread_id: Some(thread_id.clone()),
            agent_label: None,
        };

        persist_compact_summary_event(&thread_id, &event, "test/model", &persistence_tx).await;

        let mut saw_summary_event = false;
        while let Ok(event_out) = persistence_rx.try_recv() {
            if let RuntimePersistenceEvent::InsertCompactSummaryMessage {
                thread_id: event_thread_id,
                summary,
                model_ref,
            } = event_out
            {
                assert_eq!(event_thread_id, thread_id);
                assert_eq!(summary.markdown, event.summary);
                assert_eq!(model_ref, "test/model");
                saw_summary_event = true;
            }
        }
        assert!(saw_summary_event);
    }

    #[tokio::test]
    async fn approve_plan_adds_short_user_confirmation_only() {
        ensure_test_persistence().await;

        let root = unique_temp_root("approve-plan-short-message");
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).expect("failed to create cwd");
        let config = test_user_config();
        let project = ProjectsDir::new(&root)
            .for_storage_key("test-project", &config)
            .expect("failed to create project dir");
        let settings = settings_for_cwd(&config, &cwd);
        let (mut runtime, mut event_rx) = runtime_for_thread(settings, project.clone());

        runtime.messages = vec![Message::new(
            Role::Assistant,
            vec![ContentBlock::from_text(
                "<proposed_plan>\n# Approved plan\n\n- Execute it.\n</proposed_plan>".to_string(),
            )],
        )];
        cancel_next_run(&runtime);

        runtime
            .resolve_plan_approval(
                "plan",
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
            if let RuntimeToServerEvent::UserMessageInjected {
                item: HistoryItem::Message(message),
                client_echo_id: None,
            } = event
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
            .for_storage_key("test-project", &config)
            .expect("failed to create project dir");
        let settings = settings_for_cwd(&config, &cwd);
        let (mut runtime, _event_rx) = runtime_for_thread(settings, project);

        runtime.messages = vec![Message::new(
            Role::Assistant,
            vec![ContentBlock::from_text(
                "<proposed_plan>\n# Approved plan\n\n- Execute it.\n</proposed_plan>".to_string(),
            )],
        )];
        cancel_next_run(&runtime);

        runtime
            .resolve_plan_approval(
                "plan",
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
            .for_storage_key("test-project", &config)
            .expect("failed to create project dir");
        let settings = settings_for_cwd(&config, &cwd);
        let (mut runtime, mut event_rx) = runtime_for_thread(settings, project);

        runtime
            .resolve_plan_approval("plan", PlanApprovalAction::ContinueDiscussing)
            .await;

        let mut saw_resolved = false;
        while let Ok(event) = event_rx.try_recv() {
            if let RuntimeToServerEvent::PlanApprovalResolved { plan_id, action } = event {
                assert_eq!(plan_id, "plan");
                assert_eq!(action, PlanApprovalAction::ContinueDiscussing);
                saw_resolved = true;
            }
        }
        assert!(saw_resolved);
    }

    #[tokio::test]
    async fn approve_in_new_thread_only_resolves_drawer_without_changing_state() {
        // Server 路由层在收到 ApproveInNewThread 时会自行 fork 新 ThreadRuntime，
        // core 收到此 action 后只能关闭审批抽屉,不能改 active_profile、注入 plan
        // 消息或启动 run(否则会和 server 的 fork 路径重复执行)。
        ensure_test_persistence().await;

        let root = unique_temp_root("approve-in-new-thread");
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).expect("failed to create cwd");
        let config = test_user_config();
        let project = ProjectsDir::new(&root)
            .for_storage_key("test-project", &config)
            .expect("failed to create project dir");
        let settings = settings_for_cwd(&config, &cwd);
        let (mut runtime, mut event_rx) = runtime_for_thread(settings, project);

        let original_profile = runtime.active_profile();
        let original_message_count = runtime.messages.len();

        runtime
            .resolve_plan_approval(
                "plan",
                PlanApprovalAction::ApproveInNewThread {
                    profile: PlanExecutionProfile::Main,
                },
            )
            .await;

        // 状态保持:active_profile 不变,消息不增加,不应有 user message 注入事件。
        assert_eq!(runtime.active_profile(), original_profile);
        assert_eq!(runtime.messages.len(), original_message_count);

        let mut saw_resolved = false;
        let mut saw_user_message = false;
        while let Ok(event) = event_rx.try_recv() {
            match event {
                RuntimeToServerEvent::PlanApprovalResolved { plan_id, action } => {
                    assert_eq!(plan_id, "plan");
                    assert!(matches!(
                        action,
                        PlanApprovalAction::ApproveInNewThread { .. }
                    ));
                    saw_resolved = true;
                }
                RuntimeToServerEvent::UserMessageInjected { .. } => {
                    saw_user_message = true;
                }
                _ => {}
            }
        }
        assert!(saw_resolved, "resolved event should be emitted");
        assert!(
            !saw_user_message,
            "approve-in-new-thread must not inject plan message into core"
        );
    }

    #[tokio::test]
    async fn reload_subagent_registry_rebuilds_runtime_capabilities_and_prompt() {
        ensure_test_persistence().await;

        let root = unique_temp_root("reload-subagent-registry");
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).expect("failed to create cwd");
        let config = test_user_config();
        let project = ProjectsDir::new(&root)
            .for_storage_key("test-project", &config)
            .expect("failed to create project dir");
        let settings = settings_for_cwd(&config, &cwd);
        let (mut runtime, _event_rx) = runtime_for_thread(settings, project);

        assert!(
            !runtime
                .settings
                .system_prompt
                .as_deref()
                .expect("system prompt should exist")
                .contains("cache-helper")
        );
        crate::subagents::write_agent_file(
            &cwd,
            omini_domain::subagents::AgentSourceKind::Project,
            &omini_domain::subagents::AgentDraft {
                name: "cache-helper".to_string(),
                description: "Use when checking cache-sensitive changes.".to_string(),
                short_description: None,
                instructions: "Inspect cache-sensitive changes and report findings.".to_string(),
                tools: Vec::new(),
                disallow_tools: Vec::new(),
                model: None,
            },
        )
        .expect("failed to write test agent");

        runtime.reload_subagent_registry();

        assert!(
            runtime
                .capabilities
                .subagent_registry()
                .summaries()
                .iter()
                .any(|agent| agent.name == "cache-helper")
        );
        assert!(
            runtime
                .settings
                .system_prompt
                .as_deref()
                .expect("system prompt should be rebuilt")
                .contains("cache-helper")
        );
    }

    #[tokio::test]
    async fn event_processor_auto_profile_resolves_permission_pause_without_ui() {
        ensure_test_persistence().await;

        let root = unique_temp_root("auto-profile-pause-runtime");
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).expect("failed to create cwd");
        let config = test_user_config();
        let project = ProjectsDir::new(&root)
            .for_storage_key("test-project", &config)
            .expect("failed to create project dir");
        let settings = settings_for_cwd(&config, &cwd);
        let (mut runtime, mut event_rx) = runtime_for_thread(settings, project);
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
            .send(EngineToRuntimeEvent::ToolPauseRequested(Box::new(
                permission_pause("tool_1"),
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
                .any(|event| matches!(event, RuntimeToServerEvent::ToolPauseRequested(_)))
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
            .for_storage_key("test-project", &config)
            .expect("failed to create project dir");
        let settings = settings_for_cwd(&config, &cwd);
        let (runtime, mut event_rx) = runtime_for_thread(settings, project);
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
            .send(EngineToRuntimeEvent::ToolPauseRequested(Box::new(
                permission_pause("tool_1"),
            )))
            .await
            .expect("pause event should send");

        let event = tokio::time::timeout(Duration::from_secs(1), event_rx.recv())
            .await
            .expect("ui pause event should arrive")
            .expect("ui event channel should stay open");
        let RuntimeToServerEvent::ToolPauseRequested(req) = event else {
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
            .for_storage_key("test-project", &config)
            .expect("failed to create project dir");
        let settings = settings_for_cwd(&config, &cwd);
        let (runtime, mut event_rx, mut persistence_rx) =
            runtime_for_thread_with_persistence(settings, project);
        let thread_id = runtime.thread_id.clone();
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
            .send(EngineToRuntimeEvent::UserMessageProduced {
                message: message.clone(),
                client_echo_id: Some("echo-intervention".to_string()),
            })
            .await
            .expect("user message event should send");

        let event = tokio::time::timeout(Duration::from_secs(1), event_rx.recv())
            .await
            .expect("ui user message event should arrive")
            .expect("ui event channel should stay open");
        let RuntimeToServerEvent::UserMessageInjected {
            item: HistoryItem::Message(event_message),
            client_echo_id,
        } = event
        else {
            panic!("expected user message injection");
        };
        assert_eq!(event_message, message);
        assert_eq!(client_echo_id.as_deref(), Some("echo-intervention"));

        let mut saw_persistence_event = false;
        while let Ok(event) = persistence_rx.try_recv() {
            if let RuntimePersistenceEvent::InsertMessage {
                thread_id: event_thread_id,
                role,
                blocks,
                ..
            } = event
                && event_thread_id == thread_id
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
    async fn task_notification_is_visible_only_after_successful_persistence_ack() {
        for persistence_result in [Ok(()), Err("database unavailable".to_string())] {
            let root = unique_temp_root("task-notification-ack");
            let cwd = root.join("workspace");
            std::fs::create_dir_all(&cwd).expect("failed to create cwd");
            let config = test_user_config();
            let project = ProjectsDir::new(&root)
                .for_storage_key("test-project", &config)
                .expect("failed to create project dir");
            let settings = settings_for_cwd(&config, &cwd);
            let (runtime, mut event_rx, mut persistence_rx) =
                runtime_for_thread_with_persistence(settings, project);
            drain_events(&mut event_rx);
            let (engine_tx, engine_rx) = mpsc::channel(4);
            let processor = runtime
                .spawn_event_processor(
                    engine_rx,
                    ActiveProfile::Main,
                    Arc::clone(&runtime.active_profile),
                    empty_tool_pause_resolver(),
                )
                .await;
            let notification = AgentTaskNotification {
                tasks: vec![AgentTaskNotificationItem {
                    task_id: "task_1".to_string(),
                    agent: "general".to_string(),
                    title: "Test".to_string(),
                    status: AgentTaskStatus::Completed,
                }],
                created_at: chrono::Utc::now(),
            };
            let (ack, result) = tokio::sync::oneshot::channel();
            engine_tx
                .send(EngineToRuntimeEvent::AgentTaskNotificationsProduced {
                    notification: notification.clone(),
                    llm_message: Message::from_user_text("task completed".to_string()),
                    task_ids: vec!["task_1".to_string()],
                    ack,
                })
                .await
                .unwrap();

            let RuntimePersistenceEvent::InsertAgentTaskNotification { ack, .. } =
                persistence_rx.recv().await.unwrap()
            else {
                panic!("expected task notification persistence event");
            };
            assert!(event_rx.try_recv().is_err());
            let should_succeed = persistence_result.is_ok();
            ack.send(persistence_result).unwrap();
            assert_eq!(result.await.unwrap().is_ok(), should_succeed);
            if should_succeed {
                assert!(matches!(
                    event_rx.recv().await,
                    Some(RuntimeToServerEvent::UserMessageInjected {
                        item: HistoryItem::AgentTaskNotification(saved),
                        client_echo_id: None,
                    }) if saved == notification
                ));
            } else {
                tokio::task::yield_now().await;
                assert!(event_rx.try_recv().is_err());
            }
            drop(engine_tx);
            processor.await.unwrap();
        }
    }

    #[tokio::test]
    async fn split_tool_result_history_writes_image_only_to_llm_context() {
        ensure_test_persistence().await;

        let root = unique_temp_root("split-tool-result-history");
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).expect("failed to create cwd");
        let config = test_user_config();
        let project = ProjectsDir::new(&root)
            .for_storage_key("test-project", &config)
            .expect("failed to create project dir");
        let settings = settings_for_cwd(&config, &cwd);
        let (runtime, _event_rx, mut persistence_rx) =
            runtime_for_thread_with_persistence(settings, project);
        let expected_thread_id = runtime.thread_id.clone();

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

        let mut saw_display_message = false;
        let mut saw_llm_message = false;
        while let Ok(event) = persistence_rx.try_recv() {
            match event {
                RuntimePersistenceEvent::AppendLlmMessage {
                    thread_id, message, ..
                } if thread_id == expected_thread_id && message == llm_msg => {
                    saw_llm_message = true;
                }
                RuntimePersistenceEvent::InsertMessage {
                    thread_id, blocks, ..
                } if thread_id == expected_thread_id && blocks == display_msg.content => {
                    assert!(
                        !blocks
                            .iter()
                            .any(|block| matches!(block, ContentBlock::Image(_)))
                    );
                    saw_display_message = true;
                }
                _ => {}
            }
        }
        assert!(saw_llm_message);
        assert!(saw_display_message);
    }

    #[tokio::test]
    async fn usage_events_update_main_thread_totals() {
        ensure_test_persistence().await;

        let root = unique_temp_root("usage-events");
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).expect("failed to create cwd");
        let config = test_user_config();
        let project = ProjectsDir::new(&root)
            .for_storage_key("test-project", &config)
            .expect("failed to create project dir");
        let settings = settings_for_cwd(&config, &cwd);
        let (mut runtime, mut event_rx, mut persistence_rx) =
            runtime_for_thread_with_persistence(settings, project);

        runtime.messages = vec![Message::from_user_text("thread body".to_string())];
        let parent_thread_id = runtime.thread_id.clone();
        drain_events(&mut event_rx);
        while persistence_rx.try_recv().is_ok() {}

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
            .send(EngineToRuntimeEvent::UsageRecorded(Usage {
                prompt_tokens: 10,
                completion_tokens: 5,
                cached_tokens: 3,
            }))
            .await
            .expect("usage event should send");
        drop(engine_tx);
        processor.await.expect("processor should finish");

        let mut saw_parent_usage = false;
        while let Ok(event) = persistence_rx.try_recv() {
            match event {
                RuntimePersistenceEvent::RecordThreadUsage { thread_id, usage }
                    if thread_id == parent_thread_id =>
                {
                    assert_eq!(usage.total_tokens(), 15);
                    assert_eq!(usage.cached_tokens, 3);
                    saw_parent_usage = true;
                }
                _ => {}
            }
        }
        assert!(saw_parent_usage);

        let mut last_usage = None;
        while let Ok(event) = event_rx.try_recv() {
            if let RuntimeToServerEvent::UsageChanged(snapshot) = event {
                last_usage = Some(snapshot);
            }
        }
        let snapshot = last_usage.expect("usage snapshot should be emitted");
        assert_eq!(snapshot.current_context_tokens, 15);
        assert_eq!(snapshot.total_tokens, 15);
        assert_eq!(snapshot.total_cached_tokens, 3);
    }
}
