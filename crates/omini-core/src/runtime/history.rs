use crate::proposed_plan::strip_proposed_plan_blocks;
use crate::runtime::service::RunStart;
use omini_domain::conversation::{CompactionSummary, ProposedPlan};
use omini_model::message::{ContentBlock, Message, Role, TextBlock};
use omini_runtime_contract::persistence::RuntimePersistenceEvent;
use omini_runtime_contract::thread_domain::ActiveProfile;
use tokio::sync::mpsc;

pub async fn persist_initial_user_message(
    thread_id: &str,
    llm_message: Option<Message>,
    start: RunStart,
    model_ref: &str,
    persistence_tx: &mpsc::Sender<RuntimePersistenceEvent>,
) {
    let Some(llm_message) = llm_message else {
        return;
    };
    match start {
        RunStart::UserMessage => {
            persist_one(
                thread_id,
                llm_message,
                ActiveProfile::Main,
                model_ref,
                persistence_tx,
            )
            .await;
        }
        RunStart::UserInput => {
            persist_llm_history_only(thread_id, &llm_message, persistence_tx).await;
        }
        RunStart::PendingAgentTaskNotification | RunStart::PersistedAgentTaskNotification => {}
    }
}

pub async fn persist_one(
    thread_id: &str,
    msg: Message,
    active_profile: ActiveProfile,
    model_ref: &str,
    persistence_tx: &mpsc::Sender<RuntimePersistenceEvent>,
) {
    persist_llm_history_only(thread_id, &msg, persistence_tx).await;
    persist_ui_message(thread_id, &msg, active_profile, model_ref, persistence_tx).await;
}

pub async fn persist_llm_history_only(
    thread_id: &str,
    msg: &Message,
    persistence_tx: &mpsc::Sender<RuntimePersistenceEvent>,
) {
    let _ = persistence_tx
        .send(RuntimePersistenceEvent::AppendLlmMessage {
            thread_id: thread_id.to_string(),
            message: msg.clone(),
        })
        .await;
}

pub async fn persist_ui_message(
    thread_id: &str,
    msg: &Message,
    active_profile: ActiveProfile,
    model_ref: &str,
    persistence_tx: &mpsc::Sender<RuntimePersistenceEvent>,
) {
    // UI 行的 content 是按 active_profile 剥离 plan 块后的展示块；剥离必须在
    // 发射时刻完成，因为 profile 快照属于 core，server 侧无法复现。
    let _ = persistence_tx
        .send(RuntimePersistenceEvent::UiMessageAppended {
            thread_id: thread_id.to_string(),
            message: Message::new(msg.role.clone(), ui_message_blocks(msg, active_profile)),
            model_ref: model_ref_for_role(msg.role.clone(), model_ref),
        })
        .await;
}

fn model_ref_for_role(role: Role, model_ref: &str) -> Option<String> {
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

pub async fn persist_plan_ui_message(
    thread_id: &str,
    plan: &ProposedPlan,
    model_ref: &str,
    persistence_tx: &mpsc::Sender<RuntimePersistenceEvent>,
) {
    let _ = persistence_tx
        .send(RuntimePersistenceEvent::InsertPlanMessage {
            thread_id: thread_id.to_string(),
            plan: plan.clone(),
            model_ref: model_ref.to_string(),
        })
        .await;
}

pub async fn persist_compact_summary_ui_message(
    thread_id: &str,
    summary: &CompactionSummary,
    model_ref: &str,
    persistence_tx: &mpsc::Sender<RuntimePersistenceEvent>,
) {
    let _ = persistence_tx
        .send(RuntimePersistenceEvent::InsertCompactSummaryMessage {
            thread_id: thread_id.to_string(),
            summary: summary.clone(),
            model_ref: model_ref.to_string(),
        })
        .await;
}
