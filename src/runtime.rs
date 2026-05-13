use crate::api::LlmClient;
use crate::command::{self, CommandRegistry};
use crate::config::project::ProjectDir;
use crate::config::project::SessionDir;
use crate::config::project::sanitize;
use crate::db::{self, NewMessage};
use crate::engine::{QueryContext, QueryEngine};
use crate::tools::ToolRegistry;
use crate::types::config::Settings;
use crate::types::config::ThinkingEffort;
use crate::types::events::{CommandResult, EngineEvent, RuntimeEvent, UiRequest};
use crate::types::message::{Message, Role};
use chrono::Utc;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::mpsc;
use uuid::Uuid;

/// 待处理的交互类型（等待 UI 回传选择结果）。
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PendingInteraction {
    ModelSelect,
    SessionSelect,
}

/// Agent 运行时。
///
/// 维护自己的对话历史，通过 channel 与 UI 双向通信。
/// 一次 `SendMessage` 可能触发多轮 LLM 调用 + 工具执行，
/// 直到 LLM 自然结束或达到最大轮次。
pub struct AgentRuntime {
    /// 当前会话 ID（第一次提交时生成）
    pub(crate) session_id: Option<String>,
    /// 创建后缓存会话目录句柄
    pub(crate) session_dir: Option<SessionDir>,
    /// 向 UI 发送事件
    event_tx: mpsc::Sender<RuntimeEvent>,
    /// 接收 UI 发来的请求
    request_rx: mpsc::Receiver<UiRequest>,
    /// 配置
    pub(crate) settings: Settings,
    /// 当前项目目录
    pub(crate) project: ProjectDir,
    /// 运行时自主维护的对话历史
    pub(crate) messages: Vec<Message>,
    /// LLM 客户端
    llm_client: LlmClient,
    /// 查询引擎
    query_engine: QueryEngine,
    /// 工具注册表（持有所有注册的工具）
    tool_registry: Arc<ToolRegistry>,
    /// 取消标志（用于 CancelRun）
    cancelled: Arc<AtomicBool>,
    /// 命令注册表
    pub(crate) command_registry: CommandRegistry,
    /// 当前等待 UI 回传的交互类型
    pending_interaction: Option<PendingInteraction>,
}

impl AgentRuntime {
    pub fn new(
        event_tx: mpsc::Sender<RuntimeEvent>,
        request_rx: mpsc::Receiver<UiRequest>,
        settings: Settings,
        project: ProjectDir,
    ) -> Self {
        let llm_client = LlmClient::new(
            settings.endpoint,
            settings.api_key.clone(),
            settings.base_url.clone(),
        );
        let tool_registry = Arc::new(crate::tools::create_default_registry());

        // 初始化命令注册表并注册内置命令
        let mut command_registry = CommandRegistry::new();
        command::register_default_commands(&mut command_registry);

        // 向 UI 推送命令列表（供自动补全使用）
        let summaries = command_registry.summaries();
        let _ = event_tx.try_send(RuntimeEvent::CommandList(summaries));

        Self {
            session_id: None,
            session_dir: None,
            event_tx,
            request_rx,
            settings,
            project,
            messages: Vec::new(),
            cancelled: Arc::new(AtomicBool::new(false)),
            llm_client,
            tool_registry,
            query_engine: QueryEngine::new(),
            command_registry,
            pending_interaction: None,
        }
    }

    /// 启动运行时，返回 JoinHandle。
    pub fn run(mut self) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    Some(req) = self.request_rx.recv() => {
                        match req {
                            UiRequest::SendMessage(text) => {
                                // 检查是否为命令
                                if let Some(parsed) = command::parse(&text) {
                                    self.handle_command(&parsed).await;
                                } else {
                                    self.messages.push(Message::from_user_text(text));
                                    self.process_run().await;
                                }
                            }
                            UiRequest::CancelRun => {
                                self.cancelled.store(true, Ordering::Relaxed);
                                self.query_engine.cancel_current_run();
                            }
                            UiRequest::ModelSelected { provider, model, thinking_effort } => {
                                if self.pending_interaction
                                    == Some(PendingInteraction::ModelSelect)
                                {
                                    self.switch_model(&provider, &model, thinking_effort).await;
                                    self.pending_interaction = None;
                                }
                            }
                            UiRequest::SessionSelected { session_id } => {
                                if self.pending_interaction
                                    == Some(PendingInteraction::SessionSelect)
                                {
                                    self.switch_session(&session_id).await;
                                    self.pending_interaction = None;
                                }
                            }
                            UiRequest::ResolveToolPause { tool_use_id, response } => {
                                if let Err(e) = self
                                    .query_engine
                                    .resolve_tool_pause(&tool_use_id, response)
                                {
                                    self.send_event(RuntimeEvent::Error(e)).await;
                                }
                            }
                        }
                    }
                    else => break,
                }
            }
        })
    }

    /// 处理命令分发。
    async fn handle_command(&mut self, parsed: &command::ParsedCommand<'_>) {
        if let Some(cmd) = self.command_registry.get(parsed.name) {
            let cmd = Arc::clone(cmd);
            let result = cmd.execute(self, parsed.args).await;
            match result {
                CommandResult::Done => {}
                CommandResult::Pending => {
                    self.pending_interaction = match parsed.name {
                        "model" => Some(PendingInteraction::ModelSelect),
                        "sessions" | "resume" => Some(PendingInteraction::SessionSelect),
                        _ => None,
                    };
                }
                CommandResult::Error(e) => {
                    self.send_event(RuntimeEvent::Error(e)).await;
                }
            }
        } else {
            self.send_event(RuntimeEvent::CommandOutput(format!(
                "未知命令: /{}. 输入 /help 查看可用命令。",
                parsed.name
            )))
            .await;
        }
    }

    /// 切换模型 / 提供商（/model 交互完成后回调）。
    async fn switch_model(
        &mut self,
        provider: &str,
        model: &str,
        thinking_effort: Option<ThinkingEffort>,
    ) {
        if let Some(profile) = self.settings.providers.get(provider) {
            self.settings.active_provider = provider.to_string();
            self.settings.model = model.to_string();
            self.settings.thinking_effort = thinking_effort;
            self.settings.api_key = profile.api_key.clone();
            self.settings.base_url = profile.base_url.clone();
            self.settings.endpoint = profile.endpoint;

            // 重建 LLM 客户端
            self.llm_client = LlmClient::new(
                profile.endpoint,
                profile.api_key.clone(),
                profile.base_url.clone(),
            );

            // 持久化：新会话 → 项目状态；已有会话 → 数据库会话记录
            if let Some(sid) = &self.session_id {
                let te = thinking_effort.as_ref().map(|t| format!("{t:?}"));
                let _ = db::global_db()
                    .update_session_config(sid, provider, model, te.as_deref())
                    .await;
            } else if let Ok(mut state) = self.project.load_state() {
                state.default_provider = Some(provider.to_string());
                state.default_model = Some(model.to_string());
                state.thinking_effort = thinking_effort;
                let _ = self.project.save_state(&state);
            }

            // 通知 UI
            self.send_event(RuntimeEvent::ModelChanged {
                provider: provider.to_string(),
                model: model.to_string(),
                thinking_effort: self.settings.thinking_effort,
            })
            .await;
        } else {
            self.send_event(RuntimeEvent::Error(format!("提供商 '{provider}' 不存在")))
                .await;
        }
    }

    /// 切换会话（/sessions 交互完成后回调）。
    async fn switch_session(&mut self, session_id: &str) {
        self.session_id = Some(session_id.to_string());

        let db_session = match db::global_db().get_session(session_id).await {
            Ok(Some(s)) => s,
            _ => {
                self.send_event(RuntimeEvent::Error("会话不存在".to_string()))
                    .await;
                return;
            }
        };

        let session_dir = self.project.session(session_id);
        self.session_dir = Some(session_dir.clone());

        // 同步会话的提供商 / 模型到运行时（若不同则切换）
        let provider_changed = db_session.provider != self.settings.active_provider
            || db_session.model != self.settings.model;

        if provider_changed && let Some(profile) = self.settings.providers.get(&db_session.provider)
        {
            self.settings.active_provider = db_session.provider.clone();
            self.settings.model = db_session.model.clone();
            self.settings.api_key = profile.api_key.clone();
            self.settings.base_url = profile.base_url.clone();
            self.settings.endpoint = profile.endpoint;
            self.llm_client = LlmClient::new(
                profile.endpoint,
                profile.api_key.clone(),
                profile.base_url.clone(),
            );
        }

        // 同步思考程度
        let thinking_effort = db_session.thinking_effort.as_deref().and_then(|s| match s {
            "low" => Some(ThinkingEffort::Low),
            "medium" => Some(ThinkingEffort::Medium),
            "high" => Some(ThinkingEffort::High),
            _ => None,
        });
        self.settings.thinking_effort = thinking_effort;

        // 从数据库加载消息（而非 JSONL）
        let blocks_dir = session_dir.path().join("blocks");
        let messages = load_messages_from_db(session_id, &blocks_dir).await;

        let count = messages.len() as i64;
        let ui_messages = messages.clone();
        self.messages = messages;

        let _ = db::global_db()
            .update_session_msg_count(session_id, count)
            .await;

        self.send_event(RuntimeEvent::SessionTitleChanged {
            title: db_session.title,
        })
        .await;

        self.send_event(RuntimeEvent::ModelChanged {
            provider: db_session.provider.clone(),
            model: db_session.model.clone(),
            thinking_effort,
        })
        .await;

        self.send_event(RuntimeEvent::SessionChanged {
            session_id: Some(session_id.to_string()),
            messages: ui_messages,
        })
        .await;
    }

    /// 处理一次完整的用户请求（可能含多轮 LLM 调用）。
    async fn process_run(&mut self) {
        if self.session_id.is_none() {
            self.create_session().await;
        } else {
            // 已有 session，更新 updated_at 时间戳
            let id = self.session_id.as_ref().expect("session_id should exist");
            let _ = db::global_db().update_session_updated_at(id).await;
        }

        self.send_event(RuntimeEvent::RunStarted).await;

        // 本轮开始时已有的消息数，即用户消息所在的位置
        let prev_len = self.messages.len();

        // 先把用户消息持久化（这条不是引擎产生的）
        self.persist_message_at(prev_len - 1).await;

        // 创建 engine → runtime 的内部通信通道
        let (engine_tx, engine_rx) = mpsc::channel::<EngineEvent>(256);

        // 启动事件处理器（独立 task），负责增量持久化 + 转发到 UI
        let processor = self.spawn_event_processor(engine_rx).await;

        {
            // 引擎直接在当前 task 运行，&mut self.messages 零拷贝
            let ctx = QueryContext {
                messages: &mut self.messages,
                settings: &self.settings,
                llm_client: &self.llm_client,
                tool_registry: Arc::clone(&self.tool_registry),
            };

            let event_tx = self.event_tx.clone();
            let query = self
                .query_engine
                .run_query(ctx, engine_tx, Arc::clone(&self.cancelled));
            tokio::pin!(query);

            loop {
                tokio::select! {
                    result = &mut query => {
                        let _result = result;
                        break;
                    }
                    Some(req) = self.request_rx.recv() => {
                        match req {
                            UiRequest::CancelRun => {
                                self.cancelled.store(true, Ordering::Relaxed);
                                self.query_engine.cancel_current_run();
                            }
                            UiRequest::ResolveToolPause { tool_use_id, response } => {
                                if let Err(e) = self
                                    .query_engine
                                    .resolve_tool_pause(&tool_use_id, response)
                                {
                                    let _ = event_tx.send(RuntimeEvent::Error(e)).await;
                                }
                            }
                            UiRequest::SendMessage(_)
                            | UiRequest::ModelSelected { .. }
                            | UiRequest::SessionSelected { .. } => {
                                let _ = event_tx
                                    .send(RuntimeEvent::Error(
                                        "Cannot handle this request while a run is active".to_string(),
                                    ))
                                    .await;
                            }
                        }
                    }
                    else => break,
                }
            }
        }

        // 等待事件处理器自然退出（engine_tx drop 后 engine_rx 收到 None）
        let _ = processor.await;

        // 更新 session 消息计数（所有消息已被处理器增量持久化）
        let total = self.messages.len() as i64;
        db::global_db()
            .update_session_msg_count(self.session_id.as_ref().unwrap(), total)
            .await
            .expect("failed to update session message count");

        self.cancelled.store(false, Ordering::Relaxed);
        self.send_event(RuntimeEvent::RunFinished).await;
    }

    /// 启动事件处理器
    async fn spawn_event_processor(
        &self,
        mut engine_rx: mpsc::Receiver<EngineEvent>,
    ) -> tokio::task::JoinHandle<()> {
        let session_id = self
            .session_id
            .clone()
            .expect("session must exist before processing events");
        let session_dir = self
            .session_dir
            .clone()
            .expect("session dir must exist before processing events");
        let event_tx = self.event_tx.clone();
        let blocks_dir = session_dir.path().join("blocks");

        tokio::spawn(async move {
            while let Some(event) = engine_rx.recv().await {
                match event {
                    // ===== 需要持久化的事件 =====
                    EngineEvent::MessageProduced(msg) => {
                        persist_one(&session_dir, &session_id, &blocks_dir, &msg, "assistant")
                            .await;
                    }
                    EngineEvent::ToolResultsProduced(msg) => {
                        persist_one(&session_dir, &session_id, &blocks_dir, &msg, "user").await;
                    }
                    // ===== 透传事件 =====
                    EngineEvent::TurnStarted => {
                        let _ = event_tx.send(RuntimeEvent::TurnStarted).await;
                    }
                    EngineEvent::TurnEnded => {
                        let _ = event_tx.send(RuntimeEvent::TurnEnded).await;
                    }
                    EngineEvent::ThinkingDelta(t) => {
                        let _ = event_tx.send(RuntimeEvent::ThinkingDelta(t)).await;
                    }
                    EngineEvent::TextDelta(t) => {
                        let _ = event_tx.send(RuntimeEvent::TextDelta(t)).await;
                    }
                    EngineEvent::ToolUse(tu) => {
                        let _ = event_tx.send(RuntimeEvent::ToolUse(tu)).await;
                    }
                    EngineEvent::ToolResult(tr) => {
                        let _ = event_tx.send(RuntimeEvent::ToolResult(tr)).await;
                    }
                    EngineEvent::ToolPauseRequested(req) => {
                        let _ = event_tx.send(RuntimeEvent::ToolPauseRequested(req)).await;
                    }
                    EngineEvent::Error(e) => {
                        let _ = event_tx.send(RuntimeEvent::Error(e)).await;
                    }
                }
            }
        })
    }

    /// 发送事件到 UI（忽略 send 失败）
    pub(crate) async fn send_event(&self, event: RuntimeEvent) {
        let _ = self.event_tx.send(event).await;
    }

    /// 首次提交时创建 session：生成 UUID、建目录、写 DB。
    async fn create_session(&mut self) {
        let id = Uuid::new_v4().to_string();
        self.session_id = Some(id.clone());

        let session_dir = self
            .project
            .create_session(&id)
            .expect("failed to create session directory");
        self.session_dir = Some(session_dir);

        let now = Utc::now();
        let project_path = sanitize(&self.settings.cwd);
        // 从第一条用户消息中提取标题
        let title = self.messages.last().and_then(|msg| {
            msg.content.first().and_then(|block| {
                if let crate::types::message::ContentBlock::Text(t) = block {
                    let text = t.text.trim().to_string();
                    if text.is_empty() {
                        None
                    } else {
                        Some(text.chars().take(300).collect())
                    }
                } else {
                    None
                }
            })
        });
        let session = crate::db::Session {
            id,
            project_path,
            parent_session_id: None,
            session_type: "main".to_string(),
            agent_label: None,
            provider: self.settings.active_provider.clone(),
            model: self.settings.model.clone(),
            thinking_effort: self
                .settings
                .thinking_effort
                .map(|t| format!("{t:?}").to_lowercase()),
            title,
            message_count: 0,
            created_at: now,
            updated_at: now,
        };
        db::global_db()
            .create_session(&session)
            .await
            .expect("failed to persist session");

        let title_out = session.title.clone();
        let session_id_out = session.id.clone();
        self.send_event(RuntimeEvent::SessionTitleChanged { title: title_out })
            .await;
        self.send_event(RuntimeEvent::SessionChanged {
            session_id: Some(session_id_out),
            messages: self.messages.clone(),
        })
        .await;
    }

    /// 持久化 `messages[idx]` 处的单条消息（用于持久化用户输入）。
    async fn persist_message_at(&self, idx: usize) {
        if idx >= self.messages.len() {
            return;
        }
        let session_id = self
            .session_id
            .as_ref()
            .expect("session must exist before persisting");
        let session_dir = self
            .session_dir
            .as_ref()
            .expect("session dir must exist before persisting");
        let blocks_root = session_dir.path().join("blocks");
        let msg = &self.messages[idx];

        persist_one(session_dir, session_id, &blocks_root, msg, msg_role(msg)).await;
    }
}

/// 从数据库加载会话消息，解析 ContentBlock（含大块溢出文件）。
async fn load_messages_from_db(session_id: &str, blocks_dir: &std::path::Path) -> Vec<Message> {
    let stored = match db::global_db().get_messages(session_id).await {
        Ok(rows) => rows,
        Err(e) => {
            eprintln!("load_messages_from_db: {e}");
            return Vec::new();
        }
    };

    let mut messages = Vec::with_capacity(stored.len());
    for sm in stored {
        let role = match sm.role.as_str() {
            "user" => Role::User,
            "assistant" => Role::Assistant,
            _ => continue,
        };
        let content_json: Vec<serde_json::Value> = match serde_json::from_str(&sm.content) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("load_messages_from_db: parse content failed: {e}");
                continue;
            }
        };
        let blocks = match db::load_blocks(&content_json, blocks_dir) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("load_messages_from_db: load_blocks failed: {e}");
                continue;
            }
        };
        messages.push(Message::new(role, blocks));
    }
    messages
}

/// 持久化单条消息到 JSONL + SQLite。
async fn persist_one(
    session_dir: &SessionDir,
    session_id: &str,
    blocks_dir: &Path,
    msg: &Message,
    role: &str,
) {
    // JSONL
    let _ = session_dir.append_history(msg);

    // SQLite
    let new_msg = NewMessage {
        session_id: session_id.to_string(),
        role: role.to_string(),
        blocks: msg.content.clone(),
        kind: "normal".to_string(),
        created_at: Utc::now(),
        blocks_dir: blocks_dir.to_path_buf(),
    };
    let _ = db::global_db().insert_message(&new_msg).await;
}

fn msg_role(msg: &Message) -> &'static str {
    match msg.role {
        Role::User => "user",
        Role::Assistant => "assistant",
    }
}
