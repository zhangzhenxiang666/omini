use crate::execution::host::AgentHost;
use crate::proposed_plan::strip_proposed_plan_blocks;
use crate::runtime::service::RunStart;
use omini_domain::conversation::{CompactionSummary, ProposedPlan};
use omini_model::message::{ContentBlock, Message, Role, TextBlock};
use omini_runtime_contract::thread_domain::ActiveProfile;

/// 运行开始时按启动方式持久化初始用户消息：普通用户消息同时进入 LLM 历史
/// 与 UI 时间线，纯输入仅进 LLM 历史，任务通知类启动没有新消息需落盘。
/// `llm_message` 为 `None` 时直接成功返回。
pub async fn persist_initial_message(
    thread_id: &str,
    llm_message: Option<Message>,
    start: RunStart,
    host: &dyn AgentHost,
) -> Result<(), crate::execution::HostError> {
    let Some(llm_message) = llm_message else {
        return Ok(());
    };
    match start {
        RunStart::UserMessage => {
            host.append_llm_message(thread_id, &llm_message).await?;
            host.append_ui_message(thread_id, &llm_message, None)
                .await?;
        }
        RunStart::UserInput => {
            host.append_llm_message(thread_id, &llm_message).await?;
        }
        RunStart::PendingTaskNotification | RunStart::PersistedTaskNotification => {}
    }
    Ok(())
}

/// UI 行的 content 是按 active_profile 剥离 plan 块后的展示块；剥离必须在
/// 发射时刻完成，因为 profile 快照属于 core，宿主侧无法复现。
pub(crate) fn ui_display_message(msg: &Message, active_profile: ActiveProfile) -> Message {
    Message::new(msg.role, ui_message_blocks(msg, active_profile))
}

pub(crate) fn model_ref_for_role(role: Role, model_ref: &str) -> Option<String> {
    (role == Role::Assistant).then(|| model_ref.to_string())
}

fn ui_message_blocks(msg: &Message, active_profile: ActiveProfile) -> Vec<ContentBlock> {
    if msg.role != Role::Assistant || active_profile != ActiveProfile::Plan {
        return msg.content.clone();
    }
    msg.content
        .iter()
        .map(|block| match block {
            ContentBlock::Text(tb) => ContentBlock::Text(TextBlock {
                text: strip_proposed_plan_blocks(&tb.text),
            }),
            other => other.clone(),
        })
        .collect()
}

/// 计划消息持久化；失败仅记录（计划文件已写入，DB 行可由重连快照补齐）。
pub async fn persist_plan_ui_message(
    thread_id: &str,
    plan: &ProposedPlan,
    model_ref: &str,
    host: &dyn AgentHost,
) {
    if let Err(error) = host.insert_plan_message(thread_id, plan, model_ref).await {
        tracing::error!(thread_id, error = %error, "failed to persist plan message");
    }
}

pub async fn persist_compact_summary_ui_message(
    thread_id: &str,
    summary: &CompactionSummary,
    model_ref: &str,
    host: &dyn AgentHost,
) {
    if let Err(error) = host
        .insert_compact_summary(thread_id, summary, model_ref)
        .await
    {
        tracing::error!(thread_id, error = %error, "failed to persist compact summary");
    }
}
