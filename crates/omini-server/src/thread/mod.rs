use crate::{
    event::{
        replay::{RuntimeReplayBuffer, SequencedRuntimeEvent},
        status::RuntimeStatusProjection,
    },
    store::Store,
};
use omini_config::{Settings, project::ProjectDir};
use omini_core::execution::AgentHandle;
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

mod acceptance;
mod build;
mod controller;
mod core;
mod events;
mod host;
mod presence;
mod snapshot;
mod status;
mod title;
mod tool_pause;

pub use host::SessionHost;

/// `ThreadSession::builder()` 这个同步构造函数所需的全部持久化输入。
///
/// - `snapshot` 喂给 replay buffer 做去重(provider/model/title/usage
///   来自 DB 的 `Thread` 行，UI messages 与 LLM context 分别来自对应表；这是
///   replay 自己的去重需求,跟"喂 LLM"是不同路径);
/// - `thread_messages` 是从当前 `llm_messages` context version 加载、
///   最终交给 core/LLM 的消息；`build` 不再过滤或合并。
pub struct ThreadSessionInputs {
    snapshot: omini_runtime_contract::thread_domain::LoadedThread,
    thread_messages: Vec<omini_model::message::Message>,
    llm_context_version: i64,
    background_tasks: Vec<domain::task::TaskInfo>,
}

/// server 侧的一个会话：持有 core Agent 实例的命令句柄与协议投影。
///
/// 职责划分：core 实例负责执行与父子调度（经 `AgentHandle` 接收命令、经
/// 单消费者任务回吐输出）；会话负责协议事件编号/replay/status 投影、多客户
/// 端控制权协调，以及空闲无客户端时的回收。HTTP 路由拿到的 `ThreadSession`
/// 不直接操作 core 的内部循环。
pub struct ThreadSession {
    // 可克隆的 core 命令句柄；用户动作经它进入实例循环并等待确认。
    handle: AgentHandle,
    acceptances: Arc<acceptance::Acceptances>,
    // daemon thread ID，同时也是数据库、项目 thread 目录和外部 WebSocket 路由使用的稳定 ID。
    thread_id: String,
    // 当前项目的目录句柄，用于加载 thread snapshot、subagent 历史和资产文件。
    project: ProjectDir,
    // 创建会话时的项目配置快照；server 用它补充 snapshot/status 中的只读信息。
    settings: Settings,
    // thread 元数据、消息、usage 的 SQLite 存储；也是 SessionHost 的落库后端。
    db: Arc<Store>,
    // 会话协议事件经过本地 seq 编号后的广播流，WebSocket 订阅和 replay 去重都用它。
    runtime_event_tx: broadcast::Sender<SequencedRuntimeEvent>,
    // server 本地产生的协议事件入口，例如 thread title 变更；消费者任务统一编号和广播。
    server_event_inbox_tx: mpsc::UnboundedSender<client_proto::RuntimeEvent>,
    // 当前连接的 client 集合和 controller 归属；HTTP mutation 会用它做控制权检查，
    // 消费者任务也读它判断空闲会话是否可回收。
    presence: Arc<Mutex<presence::ClientPresence>>,
    // 尚未 resolve 的 tool pause id 集合；resolve API 用它保证幂等并防止重复点击。
    pending_tool_pauses: Arc<Mutex<HashSet<String>>>,
    // 从会话事件流派生的轻量状态投影，供 thread status API 快速读取。
    status_projection: Arc<Mutex<RuntimeStatusProjection>>,
    // 当前工作目录的 git 分支缓存；消费者任务在 TurnEnded 后更新，status API 查询用。
    git_branch: Arc<Mutex<Option<String>>>,
    // 尚未被 snapshot 或持久化覆盖的运行中事件尾部，用于 WebSocket 重连补发。
    replay_buffer: Arc<Mutex<RuntimeReplayBuffer>>,
    // controller 变化广播流；WebSocket 连接用它同步观察者/控制者状态。
    controller_tx: broadcast::Sender<Option<String>>,
    // core 输出的唯一消费者：协议转换、seq/replay/status/tool pause 维护与广播，
    // 并在空闲无客户端时回收实例。
    _consumer_handle: JoinHandle<()>,
    consumer_finished: tokio::sync::watch::Receiver<bool>,
}

impl ThreadSessionInputs {
    pub fn new(
        snapshot: omini_runtime_contract::thread_domain::LoadedThread,
        thread_messages: Vec<omini_model::message::Message>,
        llm_context_version: i64,
    ) -> Self {
        Self {
            snapshot,
            thread_messages,
            llm_context_version,
            background_tasks: Vec::new(),
        }
    }

    pub fn with_background_tasks(mut self, tasks: Vec<domain::task::TaskInfo>) -> Self {
        self.background_tasks = tasks;
        self
    }
}
