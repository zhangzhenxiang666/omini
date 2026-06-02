use super::service::RunStart;
use crate::config::project::SessionDir;
use crate::persistence::RuntimePersistenceEvent;
use crate::types::display::{DisplayMessage, DisplayPlan, DisplaySummary, HistoryItem};
use crate::types::message::Message;
use chrono::Utc;
use std::path::Path;
use tokio::sync::mpsc;

pub(super) fn title_text(
    initial_display_message: Option<&HistoryItem>,
    fallback_message: Option<&Message>,
) -> Option<String> {
    initial_display_message
        .and_then(history_item_text)
        .or_else(|| fallback_message.and_then(message_title_text))
}

fn history_item_text(item: &HistoryItem) -> Option<String> {
    match item {
        HistoryItem::Message(message) => message_title_text(message),
        HistoryItem::Display(display) => {
            let text = display.text.trim();
            (!text.is_empty()).then(|| text.to_string())
        }
        HistoryItem::Plan(plan) => {
            let title = plan.title.trim();
            (!title.is_empty()).then(|| title.to_string())
        }
        HistoryItem::Summary(summary) => {
            let title = summary.title.trim();
            (!title.is_empty()).then(|| title.to_string())
        }
    }
}

fn message_title_text(message: &Message) -> Option<String> {
    message.content.first().and_then(|block| {
        if let crate::types::message::ContentBlock::Text(t) = block {
            let text = t.text.trim().to_string();
            if text.is_empty() { None } else { Some(text) }
        } else {
            None
        }
    })
}

pub(super) async fn persist_initial_user_message(
    session_id: Option<&str>,
    session_dir: Option<&SessionDir>,
    llm_message: Option<Message>,
    start: RunStart,
    persistence_tx: &mpsc::Sender<RuntimePersistenceEvent>,
) {
    let Some(session_id) = session_id else {
        return;
    };
    let Some(session_dir) = session_dir else {
        return;
    };
    let Some(llm_message) = llm_message else {
        return;
    };

    let blocks_dir = session_dir.path().join("blocks");
    match start {
        RunStart::UserMessage => {
            persist_one(
                session_dir,
                session_id,
                &blocks_dir,
                llm_message,
                persistence_tx,
            )
            .await;
        }
        RunStart::SplitDisplayMessage { display_message } => {
            persist_split_display_message(
                session_dir,
                session_id,
                llm_message,
                display_message,
                persistence_tx,
            )
            .await;
        }
        RunStart::Continue => {}
    }
}

pub(super) async fn persist_one(
    session_dir: &SessionDir,
    session_id: &str,
    blocks_dir: &Path,
    msg: Message,
    persistence_tx: &mpsc::Sender<RuntimePersistenceEvent>,
) {
    let _ = session_dir.append_history(&msg);
    persist_ui_message(session_id, blocks_dir, &msg, persistence_tx).await;
}

pub(super) fn persist_llm_history_only(session_dir: &SessionDir, msg: &Message) {
    let _ = session_dir.append_history(msg);
}

async fn persist_split_display_message(
    session_dir: &SessionDir,
    session_id: &str,
    llm_msg: Message,
    display_msg: DisplayMessage,
    persistence_tx: &mpsc::Sender<RuntimePersistenceEvent>,
) {
    let _ = session_dir.append_history(&llm_msg);
    let _ = persistence_tx
        .send(RuntimePersistenceEvent::InsertDisplayMessage {
            session_id: session_id.to_string(),
            display: display_msg,
            created_at: Utc::now(),
        })
        .await;
}

pub(super) async fn persist_ui_message(
    session_id: &str,
    blocks_dir: &Path,
    msg: &Message,
    persistence_tx: &mpsc::Sender<RuntimePersistenceEvent>,
) {
    let _ = persistence_tx
        .send(RuntimePersistenceEvent::InsertMessage {
            session_id: session_id.to_string(),
            role: msg.role.to_string(),
            blocks: msg.content.clone(),
            kind: "normal".to_string(),
            created_at: Utc::now(),
            blocks_dir: blocks_dir.to_path_buf(),
        })
        .await;
}

pub(super) async fn persist_plan_ui_message(
    session_id: &str,
    plan: &DisplayPlan,
    persistence_tx: &mpsc::Sender<RuntimePersistenceEvent>,
) {
    let _ = persistence_tx
        .send(RuntimePersistenceEvent::InsertPlanMessage {
            session_id: session_id.to_string(),
            plan: plan.clone(),
        })
        .await;
}

pub(super) async fn persist_compact_summary_ui_message(
    session_id: &str,
    summary: &DisplaySummary,
    persistence_tx: &mpsc::Sender<RuntimePersistenceEvent>,
) {
    let _ = persistence_tx
        .send(RuntimePersistenceEvent::InsertCompactSummaryMessage {
            session_id: session_id.to_string(),
            summary: summary.clone(),
        })
        .await;
}
