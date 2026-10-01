//! Agent 实例命令：宿主动作以请求＋确认的成对形式进入实例。
//!
//! 每个命令都携带 oneshot 确认端；确认语义为"实例已受理并完成该命令承诺的
//! 立即效果"（提交受理、配置已应用且持久化、干预已入队、取消信号已生效），
//! 运行的最终状态仍通过输出事件报告。

use crate::CoreError;
use omini_domain::config::ThinkingEffort;
use omini_model::message::Message;
use omini_runtime_contract::thread_domain::{
    ActiveProfile, ClientMessage, PlanApprovalAction, ToolPauseResponse,
};
use tokio::sync::oneshot;

type Ack<T> = oneshot::Sender<Result<T, CoreError>>;

/// 宿主发给 Agent 实例的命令；由实例任务按到达顺序处理。
#[derive(Debug)]
pub(crate) enum AgentCommand {
    /// 启动一次已预留的运行。`run_id` 来自预留阶段，展示行与初始 Run 记录
    /// 已由宿主持久化；实例据此进入执行且不再重复落 Run 创建。
    CommitRun {
        run_id: String,
        message: Message,
        ack: Ack<()>,
    },
    /// 运行中插话或子 Run 投递；`None` 指当前主 Run。实际注入发生在安全输入边界。
    Intervene {
        run_id: Option<String>,
        message: Message,
        client_source: Option<ClientMessage>,
        ack: Ack<()>,
    },
    /// 取消主 Run（及其任务树）或指定子 Run（及其后代）。
    Cancel {
        run_id: Option<String>,
        ack: Ack<()>,
    },
    CompactContext {
        instructions: Option<String>,
        ack: Ack<()>,
    },
    SetModel {
        provider: String,
        model: String,
        thinking_effort: Option<ThinkingEffort>,
        ack: Ack<()>,
    },
    SetThinkingEffort {
        effort: ThinkingEffort,
        ack: Ack<()>,
    },
    SetActiveProfile {
        profile: ActiveProfile,
        ack: Ack<()>,
    },
    ToggleActiveProfile {
        ack: Ack<()>,
    },
    ResolveToolPause {
        tool_use_id: String,
        response: ToolPauseResponse,
        ack: Ack<()>,
    },
    ResolvePlanApproval {
        plan_id: String,
        action: PlanApprovalAction,
        ack: Ack<()>,
    },
    ReloadSubagentRegistry {
        ack: Ack<()>,
    },
    /// 停止接收新工作，取消前台运行与任务树并收尾；幂等，运行中不拒绝。
    Close {
        ack: Ack<()>,
    },
}
