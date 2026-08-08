use crate::{
    event::{
        replay::{RuntimeReplayBuffer, SequencedRuntimeEvent},
        status::RuntimeStatusProjection,
    },
    store::Database,
};
use omini_config::{Settings, project::ProjectDir};
use omini_core::AgentCoreThread;
use omini_domain as domain;
use omini_protocol as client_proto;
use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
};
use tokio::{
    sync::{broadcast, mpsc},
    task::JoinHandle,
};

mod build;
mod controller;
mod core;
mod events;
mod preferences;
mod presence;
mod snapshot;
mod status;
mod title;
mod tool_pause;

/// `ThreadRuntime::build` 这个同步构造函数所需的全部持久化输入。
///
/// - `snapshot` 喂给 replay buffer 做去重(provider/model/title/usage
///   来自 DB 的 `Thread` 行，UI messages 与 LLM context 分别来自对应表；这是
///   replay 自己的去重需求,跟"喂 LLM"是不同路径);
/// - `thread_messages` 是从当前 `llm_messages` context version 加载、
///   最终交给 core/LLM 的消息；`build` 不再过滤或合并。
pub struct ThreadRuntimeInputs {
    snapshot: domain::events::LoadedSession,
    thread_messages: Vec<domain::message::Message>,
    llm_context_version: i64,
}

/// projection 和 replay buffer。HTTP 路由拿到的 `ThreadRuntime` 不直接操作 core 的内部
/// loop，而是通过这个类型做 daemon 级的持久化、重连补发和多客户端控制权协调。
pub struct ThreadRuntime {
    // 单个 daemon thread 对应的 core facade；HTTP/controller 校验后的用户动作从这里进入 core。
    core: AgentCoreThread,
    // daemon thread ID，同时也是数据库、项目 thread 目录和外部 WebSocket 路由使用的稳定 ID。
    thread_id: String,
    // 当前项目的目录句柄，用于加载 thread snapshot、subagent 历史和资产文件。
    project: ProjectDir,
    // 创建 runtime 时的项目配置快照；server 用它补充 snapshot/status 中的只读信息。
    settings: Settings,
    // session 元数据、消息、usage 和 core persistence event 的 SQLite 存储。
    db: Arc<Database>,
    // core runtime 事件经过本地 seq 编号后的广播流，WebSocket 订阅和 replay 去重都用它。
    runtime_event_tx: broadcast::Sender<SequencedRuntimeEvent>,
    // server 本地产生的协议事件入口，例如 session title 变更；fanout 会统一编号和广播。
    server_event_inbox_tx: mpsc::UnboundedSender<client_proto::RuntimeEvent>,
    // 当前连接的 client 集合和 controller 归属；HTTP mutation 会用它做控制权检查。
    presence: Mutex<presence::ClientPresence>,
    // 尚未 resolve 的 tool pause id 集合；resolve API 用它保证幂等并防止重复点击。
    pending_tool_pauses: Arc<Mutex<HashSet<String>>>,
    // 从 runtime 事件流派生的轻量状态投影，供 session status API 快速读取。
    status_projection: Arc<Mutex<RuntimeStatusProjection>>,
    // 当前工作目录的 git 分支缓存；fanout task 在 TurnEnded 后更新，status API 查询用。
    git_branch: Arc<Mutex<Option<String>>>,
    // 尚未被 snapshot 或持久化覆盖的运行中事件尾部，用于 WebSocket 重连补发。
    replay_buffer: Arc<Mutex<RuntimeReplayBuffer>>,
    // controller 变化广播流；WebSocket 连接用它同步观察者/控制者状态。
    controller_tx: broadcast::Sender<Option<String>>,
    // core 持久化事件任务：落 SQLite，成功后裁剪 replay buffer 中已持久化的尾部事件。
    _persistence_handle: JoinHandle<()>,
    // core runtime 事件任务：分配本地 seq，更新 replay/status，再广播给 WebSocket 层。
    _runtime_event_handle: JoinHandle<()>,
    // tool pause 跟踪任务：监听 runtime 事件并维护 pending_tool_pauses 集合。
    _tool_pause_handle: JoinHandle<()>,
}

impl ThreadRuntimeInputs {
    pub fn new(
        snapshot: domain::events::LoadedSession,
        thread_messages: Vec<domain::message::Message>,
        llm_context_version: i64,
    ) -> Self {
        Self {
            snapshot,
            thread_messages,
            llm_context_version,
        }
    }
}
