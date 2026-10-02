use crate::agent::AgentTaskSupervisor;
use crate::engine::QueryEngine;
use crate::execution::handle::{InstanceState, OutputHandle, RunGate};
use crate::execution::host::AgentHost;
use crate::mcp::McpManager;
use crate::runtime::command::AgentCommand;
use crate::tools::ToolRegistry;
use omini_config::Settings;
use omini_config::project::{ProjectDir, ThreadDir};
use omini_domain::task::{TaskCompletion, TaskInfo};
use omini_model::message::Message;
use omini_permissions::PermissionEngine;
use omini_provider_api::LlmClient;
use omini_runtime_contract::RuntimeToServerEvent;
use omini_runtime_contract::thread_domain::{ActiveProfile, AgentTaskInfo, ThreadUsageSnapshot};
use std::sync::atomic::{AtomicBool, AtomicI64};
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::mpsc;

pub struct RuntimeCapabilityHandles {
    pub mcp_manager: Arc<McpManager>,
    pub capabilities: Arc<crate::runtime::capabilities::CapabilityStore>,
}

impl RuntimeCapabilityHandles {
    pub fn load(settings: &Settings) -> Self {
        Self {
            mcp_manager: Arc::new(McpManager::from_settings(settings)),
            capabilities: Arc::new(crate::runtime::capabilities::CapabilityStore::load(
                settings,
            )),
        }
    }
}

/// `AgentRuntime::assemble` 的装配输入。
pub struct AgentRuntimeParts {
    pub settings: Settings,
    pub project: ProjectDir,
    pub thread_id: String,
    pub thread_dir: ThreadDir,
    pub messages: Vec<Message>,
    pub llm_context_version: i64,
    pub usage: ThreadUsageSnapshot,
    pub agent_tasks: Vec<AgentTaskInfo>,
    pub background_tasks: Vec<TaskInfo>,
    pub host: Arc<dyn AgentHost>,
    pub output: OutputHandle,
    pub cmd_rx: mpsc::Receiver<AgentCommand>,
    pub gate: Arc<RunGate>,
    pub state: Arc<InstanceState>,
    pub handles: RuntimeCapabilityHandles,
}

#[derive(Debug, Clone, Copy)]
pub enum RunStart {
    /// 启动前将最新 runtime 消息同时写入 LLM 历史和 UI 历史。
    UserMessage,
    /// 用户消息的展示行与 echo 已由宿主处理，启动前只需追加 LLM 上下文行。
    UserInput,
    /// 由待持久化的 Agent task completion 启动；落库前禁止请求 provider。
    PendingTaskNotification,
    /// 通知已在上一个 query 的终止边界持久化，只需继续请求 provider。
    PersistedTaskNotification,
}

impl RunStart {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::UserMessage => "user_message",
            Self::UserInput => "user_input",
            Self::PendingTaskNotification => "pending_task_notification",
            Self::PersistedTaskNotification => "persisted_task_notification",
        }
    }
}

/// Agent 执行内核：实例任务独占的执行状态。
///
/// 由 [`crate::execution::instance::AgentInstance`] 装配并驱动；通过 `host` 落库、
/// 通过 `output` 发布领域事件，配置与生命周期变化同步发布到共享 `state`，
/// 供 [`crate::execution::AgentHandle`] 查询。
pub(crate) struct AgentRuntime {
    pub thread_id: String,
    pub thread_dir: ThreadDir,
    /// 实例输出；发送失败即输出断开，实例进入收尾。
    pub output: OutputHandle,
    /// 执行持久化与资源申请宿主。
    pub host: Arc<dyn AgentHost>,
    /// 宿主命令接收端。
    pub cmd_rx: mpsc::Receiver<AgentCommand>,
    /// 外部提交的运行资格闸门（与句柄共享）。
    pub gate: Arc<RunGate>,
    /// 快照共享状态（与句柄共享）。
    pub state: Arc<InstanceState>,
    /// 运行时配置；每次修改后发布到 `state`。
    pub settings: Settings,
    /// 当前项目目录。
    pub project: ProjectDir,
    /// 运行时自主维护的对话历史。
    pub messages: Vec<Message>,
    pub llm_context_version: Arc<AtomicI64>,
    /// LLM 客户端。
    pub llm_client: LlmClient,
    /// 查询引擎。
    pub query_engine: QueryEngine,
    /// 工具注册表，持有所有注册的工具。
    pub tool_registry: Arc<ToolRegistry>,
    /// 从有效配置加载的 MCP 服务管理器。
    pub mcp_manager: Arc<McpManager>,
    /// runtime 是否已在 query 前等待过 MCP 启动。
    pub mcp_initialized: bool,
    pub mcp_initialization: Option<tokio::task::JoinHandle<()>>,
    pub initial_diagnostics: Vec<RuntimeToServerEvent>,
    pub deferred_start: Option<RunStart>,
    /// 长期存活的后台 task supervisor，不依赖前台运行生命周期。
    pub task_supervisor: Arc<AgentTaskSupervisor>,
    pub task_completion_rx: mpsc::UnboundedReceiver<TaskCompletion>,
    /// runtime 管理的能力注册状态；每次 query 开始时生成只读快照。
    pub capabilities: Arc<crate::runtime::capabilities::CapabilityStore>,
    /// 取消标志，用于 CancelRun 与关闭。
    pub cancelled: Arc<AtomicBool>,
    /// 当前活跃 profile，与 `state` 共享，供运行中事件处理器同步读取。
    pub active_profile: Arc<RwLock<ActiveProfile>>,
    /// 当前 thread 的 usage 快照；落库由 host 处理。
    pub thread_usage: Arc<Mutex<ThreadUsageSnapshot>>,
}

impl AgentRuntime {
    /// 装配执行内核：构建提示词、权限引擎、查询引擎与任务监督器。
    pub(crate) fn assemble(parts: AgentRuntimeParts) -> Self {
        let AgentRuntimeParts {
            mut settings,
            project,
            thread_id,
            thread_dir,
            messages,
            llm_context_version,
            usage,
            agent_tasks,
            background_tasks,
            host,
            output,
            cmd_rx,
            gate,
            state,
            handles,
        } = parts;
        let mcp_manager = handles.mcp_manager;
        let capabilities = handles.capabilities;
        let active_profile = state.active_profile_handle();
        let model = settings.active_model();
        let llm_client = LlmClient::new(
            model.protocol,
            model
                .api_key
                .as_ref()
                .map(|secret| secret.expose().to_string())
                .unwrap_or_default(),
            model.base_url.clone(),
        );
        let tool_registry = Arc::new(crate::tools::create_main_registry());
        let subagent_registry = capabilities.subagent_registry();
        let skill_registry = capabilities.skill_registry();
        settings.system_prompt = Some(crate::prompts::build_system_prompt_with_capabilities(
            &settings,
            &subagent_registry.summaries(),
            &skill_registry.injected_summaries(),
            *active_profile.read().expect("active profile lock poisoned"),
        ));
        let permission_sources = omini_config::permissions::load_permission_sources(
            &settings.cwd,
            dirs::home_dir().as_deref(),
            settings.permissions(),
        );
        let permission_engine = Arc::new(PermissionEngine::from_sources(
            settings.cwd.clone(),
            dirs::home_dir(),
            permission_sources,
        ));
        // 待交互暂停点复用 state 持有的共享 map，句柄快照因此能看到实时集合。
        let query_engine = QueryEngine::with_shared_pauses_for_main(
            Arc::clone(state.pending_tool_pauses()),
            Arc::clone(&permission_engine),
        );
        let thread_usage = Arc::new(Mutex::new(usage));
        let (task_completion_tx, task_completion_rx) = mpsc::unbounded_channel();
        let task_supervisor = AgentTaskSupervisor::builder()
            .output(output.clone())
            .host(Arc::clone(&host))
            .completion_tx(task_completion_tx)
            .pending_tool_pauses(Arc::clone(state.pending_tool_pauses()))
            .permission_engine(Arc::clone(&permission_engine))
            .active_profile(Arc::clone(&active_profile))
            .owner_usage(Arc::clone(&thread_usage))
            .initial_tasks(agent_tasks)
            .background_tasks(background_tasks)
            .build();
        task_supervisor.set_parent_inbox(query_engine.shared_user_messages());
        state.attach_supervisor(Arc::clone(&task_supervisor));

        // 装配只收集诊断，启动后顺序发送；诊断数量不受输出通道容量限制。
        let mut initial_diagnostics = Vec::new();
        for diagnostic in &subagent_registry.diagnostics {
            initial_diagnostics.push(RuntimeToServerEvent::warning(format!(
                "Subagent: {}",
                diagnostic.message()
            )));
        }
        for diagnostic in &skill_registry.diagnostics {
            initial_diagnostics.push(RuntimeToServerEvent::warning(format!(
                "Skill: {}",
                diagnostic.message()
            )));
        }
        for diagnostic in permission_engine.diagnostics() {
            initial_diagnostics.push(RuntimeToServerEvent::warning(format!(
                "Permission: {diagnostic}"
            )));
        }

        Self {
            thread_id,
            thread_dir,
            output,
            host,
            cmd_rx,
            gate,
            state,
            settings,
            project,
            messages,
            llm_context_version: Arc::new(AtomicI64::new(llm_context_version)),
            cancelled: Arc::new(AtomicBool::new(false)),
            llm_client,
            tool_registry,
            mcp_manager,
            mcp_initialized: false,
            mcp_initialization: None,
            initial_diagnostics,
            deferred_start: None,
            task_supervisor,
            task_completion_rx,
            capabilities,
            query_engine,
            active_profile,
            thread_usage,
        }
    }

    pub fn set_active_profile(&mut self, profile: ActiveProfile) {
        *self
            .active_profile
            .write()
            .expect("active profile lock poisoned") = profile;
        self.rebuild_system_prompt();
    }

    pub fn active_profile(&self) -> ActiveProfile {
        *self
            .active_profile
            .read()
            .expect("active profile lock poisoned")
    }

    /// 配置修改后的统一发布：镜像到共享状态，保证句柄查询读到实际值。
    pub(crate) fn publish_config(&self) {
        self.state.publish_settings(&self.settings);
    }
}
