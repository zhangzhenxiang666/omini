use crate::runtime::service::RunStart;
use chrono::Utc;
use omini_domain::display::{DisplayMessage, DisplayPlan, DisplaySummary};
use omini_domain::events::ActiveProfile;
use omini_domain::message::{ContentBlock, Message, Role, TextBlock};
use omini_domain::proposed_plan::strip_proposed_plan_blocks;
use omini_runtime_contract::persistence::RuntimePersistenceEvent;
use tokio::sync::mpsc;

pub(super) async fn persist_initial_user_message(
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
        RunStart::SplitDisplayMessage { display_message } => {
            persist_split_display_message(
                thread_id,
                llm_message,
                display_message,
                model_ref,
                persistence_tx,
            )
            .await;
        }
    }
}

pub(super) async fn persist_one(
    thread_id: &str,
    msg: Message,
    active_profile: ActiveProfile,
    model_ref: &str,
    persistence_tx: &mpsc::Sender<RuntimePersistenceEvent>,
) {
    persist_llm_history_only(thread_id, &msg, persistence_tx).await;
    persist_ui_message(thread_id, &msg, active_profile, model_ref, persistence_tx).await;
}

pub(super) async fn persist_llm_history_only(
    thread_id: &str,
    msg: &Message,
    persistence_tx: &mpsc::Sender<RuntimePersistenceEvent>,
) {
    let _ = persistence_tx
        .send(RuntimePersistenceEvent::AppendLlmMessage {
            thread_id: thread_id.to_string(),
            message: msg.clone(),
            created_at: Utc::now(),
        })
        .await;
}

async fn persist_split_display_message(
    thread_id: &str,
    llm_msg: Message,
    display_msg: DisplayMessage,
    model_ref: &str,
    persistence_tx: &mpsc::Sender<RuntimePersistenceEvent>,
) {
    persist_llm_history_only(thread_id, &llm_msg, persistence_tx).await;
    let _ = persistence_tx
        .send(RuntimePersistenceEvent::InsertDisplayMessage {
            thread_id: thread_id.to_string(),
            model_ref: model_ref_for_role(display_msg.role.clone(), model_ref),
            display: display_msg,
            created_at: Utc::now(),
        })
        .await;
}

pub(super) async fn persist_ui_message(
    thread_id: &str,
    msg: &Message,
    active_profile: ActiveProfile,
    model_ref: &str,
    persistence_tx: &mpsc::Sender<RuntimePersistenceEvent>,
) {
    let _ = persistence_tx
        .send(RuntimePersistenceEvent::InsertMessage {
            thread_id: thread_id.to_string(),
            role: msg.role.to_string(),
            model_ref: model_ref_for_role(msg.role.clone(), model_ref),
            blocks: ui_message_blocks(msg, active_profile),
            kind: "normal".to_string(),
            created_at: Utc::now(),
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

pub(super) async fn persist_plan_ui_message(
    thread_id: &str,
    plan: &DisplayPlan,
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

pub(super) async fn persist_compact_summary_ui_message(
    thread_id: &str,
    summary: &DisplaySummary,
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
