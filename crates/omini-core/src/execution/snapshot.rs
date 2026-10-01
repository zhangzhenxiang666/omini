//! Agent 实例的权威快照。
//!
//! 所有对外查询（线程状态、可回收性、待交互信息）都读取实例实际采用的
//! 状态；server 侧的协议投影仅用于展示，不作为运行回收的事实来源。

use omini_domain::config::ThinkingEffort;
use omini_runtime_contract::thread_domain::ActiveProfile;

/// 实例生命周期阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentLifecycle {
    /// 正常服务中：可接受新运行与命令。
    Active,
    /// 已发起关闭：不再接受新工作，正在取消运行、收尾任务与尾部输出。
    Closing,
    /// 执行任务与必要收尾已完成，终止公告已入输出。
    Closed,
}

/// 当前运行的简要状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrentRun {
    pub run_id: crate::execution::RunId,
}

/// Agent 实例快照：生命周期、运行、有效配置、待交互与空闲判定。
#[derive(Debug, Clone)]
pub struct AgentSnapshot {
    pub lifecycle: AgentLifecycle,
    /// 当前前台运行；内部续跑同样占用运行资格。
    pub current_run: Option<CurrentRun>,
    /// 是否存在已预留未提交的新运行。
    pub run_reserved: bool,
    /// 尚未进入终态的后台/子任务是否存在。
    pub has_active_tasks: bool,
    /// 维护操作或待处理完成通知等尚未结算的实例工作。
    pub has_pending_work: bool,
    /// 待交互（权限/Ask）的 tool use ID 集合，含子任务暂停点。
    pub pending_tool_pauses: Vec<String>,
    pub active_profile: ActiveProfile,
    pub provider: String,
    pub model: String,
    pub thinking_effort: Option<ThinkingEffort>,
}

impl AgentSnapshot {
    /// 实例是否可回收：无预留、无运行、无未完成任务且不在收尾。
    ///
    /// 该判定直接来自执行实例的实际状态，不依赖固定延时或客户端投影。
    pub fn is_reclaimable(&self) -> bool {
        self.lifecycle == AgentLifecycle::Active
            && self.current_run.is_none()
            && !self.run_reserved
            && !self.has_active_tasks
            && !self.has_pending_work
    }
}
