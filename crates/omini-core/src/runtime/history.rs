use super::service::RunStart;
use chrono::Utc;
use omini_config::project::SessionDir;
use omini_domain::display::{DisplayMessage, DisplayPlan, DisplaySummary};
use omini_domain::events::ActiveProfile;
use omini_domain::message::{ContentBlock, Message, Role, TextBlock};
use omini_domain::proposed_plan::strip_proposed_plan_blocks;
use omini_runtime_api::persistence::RuntimePersistenceEvent;
use std::path::Path;
use tokio::sync::mpsc;

pub(super) async fn persist_initial_user_message(
    session_id: &str,
    session_dir: &SessionDir,
    llm_message: Option<Message>,
    start: RunStart,
    persistence_tx: &mpsc::Sender<RuntimePersistenceEvent>,
) {
    let Some(llm_message) = llm_message else {
        return;
    };

    let blocks_dir = session_dir.path().join("blocks");
    match start {
        RunStart::UserMessage => {
            // 显式声明 Main:llm_message 永远是 User role,strip 内部会因角色不匹配直接走 no-op;
            // 不依赖调用方传过来的 active_profile。
            persist_one(
                session_dir,
                session_id,
                &blocks_dir,
                llm_message,
                ActiveProfile::Main,
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
    }
}

pub(super) async fn persist_one(
    session_dir: &SessionDir,
    session_id: &str,
    blocks_dir: &Path,
    msg: Message,
    active_profile: ActiveProfile,
    persistence_tx: &mpsc::Sender<RuntimePersistenceEvent>,
) {
    if let Err(error) = session_dir.append_history(&msg) {
        tracing::warn!(msg = "failed to append history", error = %error);
    }
    persist_ui_message(session_id, blocks_dir, &msg, active_profile, persistence_tx).await;
}

pub(super) fn persist_llm_history_only(session_dir: &SessionDir, msg: &Message) {
    if let Err(error) = session_dir.append_history(msg) {
        tracing::warn!(msg = "failed to append history", error = %error);
    }
}

async fn persist_split_display_message(
    session_dir: &SessionDir,
    session_id: &str,
    llm_msg: Message,
    display_msg: DisplayMessage,
    persistence_tx: &mpsc::Sender<RuntimePersistenceEvent>,
) {
    if let Err(error) = session_dir.append_history(&llm_msg) {
        tracing::warn!(msg = "failed to append history", error = %error);
    }
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
    active_profile: ActiveProfile,
    persistence_tx: &mpsc::Sender<RuntimePersistenceEvent>,
) {
    let blocks = ui_message_blocks(msg, active_profile);
    let _ = persistence_tx
        .send(RuntimePersistenceEvent::InsertMessage {
            session_id: session_id.to_string(),
            role: msg.role.to_string(),
            blocks,
            kind: "normal".to_string(),
            created_at: Utc::now(),
            blocks_dir: blocks_dir.to_path_buf(),
        })
        .await;
}

/// 为 `Message` 构造写入 UI/SQLite 持久化的内容块。
///
/// 在 `ActiveProfile::Plan` 下,Assistant 文本块中的 `<proposed_plan>` 段会被剥离,
/// 避免普通聊天面板与计划面板内容重复;计划正文仍由 `persist_plan_ui_message`
/// 单独落 `kind=plan`,LLM JSONL 历史也保留原文。
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
