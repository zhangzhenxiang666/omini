use super::service::RunStart;
use crate::config::project::{ProjectDir, SessionDir};
use crate::db::{self, NewMessage};
use crate::types::display::{DisplayMessage, DisplayPlan, DisplaySummary, HistoryItem};
use crate::types::events::{SubagentSnapshot, SubagentStatus};
use crate::types::message::{ContentBlock, Message, Role};
use crate::types::proposed_plan::{extract_proposed_plan_text, strip_proposed_plan_blocks};
use chrono::{DateTime, Utc};
use std::collections::HashSet;
use std::path::Path;

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

/// 从数据库加载会话消息，解析 ContentBlock（含大块溢出文件）或 UI display 消息。
pub(super) async fn load_messages_from_db(session_id: &str, blocks_dir: &Path) -> Vec<HistoryItem> {
    let stored = match db::global_db().get_messages(session_id).await {
        Ok(rows) => rows,
        Err(e) => {
            eprintln!("load_messages_from_db: {e}");
            return Vec::new();
        }
    };

    let persisted_plan_markdowns = stored
        .iter()
        .filter(|sm| sm.kind == "plan")
        .filter_map(|sm| serde_json::from_str::<DisplayPlan>(&sm.content).ok())
        .map(|plan| normalized_plan_markdown(&plan.markdown))
        .collect::<HashSet<_>>();

    let mut messages = Vec::with_capacity(stored.len());
    for sm in stored {
        if sm.kind == "display" {
            match serde_json::from_str::<DisplayMessage>(&sm.content) {
                Ok(display) => messages.push(HistoryItem::Display(display)),
                Err(e) => eprintln!("load_messages_from_db: parse display failed: {e}"),
            }
            continue;
        }

        if sm.kind == "plan" {
            match serde_json::from_str::<DisplayPlan>(&sm.content) {
                Ok(plan) => messages.push(HistoryItem::Plan(plan)),
                Err(e) => eprintln!("load_messages_from_db: parse plan failed: {e}"),
            }
            continue;
        }

        if sm.kind == "compact_summary" {
            match serde_json::from_str::<DisplaySummary>(&sm.content) {
                Ok(summary) => messages.push(HistoryItem::Summary(summary)),
                Err(e) => eprintln!("load_messages_from_db: parse compact summary failed: {e}"),
            }
            continue;
        }

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
        let (blocks, legacy_plans) = split_embedded_plan_blocks(
            blocks,
            role.clone(),
            &persisted_plan_markdowns,
            sm.id,
            sm.created_at,
        );
        if !blocks.is_empty() {
            messages.push(HistoryItem::Message(Message::new(role, blocks)));
        }
        messages.extend(legacy_plans.into_iter().map(HistoryItem::Plan));
    }
    messages
}

pub(super) async fn load_subagents_for_session(
    session_id: &str,
    project: &ProjectDir,
) -> Vec<SubagentSnapshot> {
    let sessions = match db::global_db().list_child_sessions(session_id).await {
        Ok(sessions) => sessions,
        Err(e) => {
            eprintln!("load_subagents_for_session: {e}");
            return Vec::new();
        }
    };

    let mut subagents = Vec::with_capacity(sessions.len());
    for session in sessions {
        let Some(parent_session_id) = session.parent_session_id.clone() else {
            continue;
        };
        let Some(spawn_tool_use_id) = session.spawn_tool_use_id.clone() else {
            continue;
        };
        let parent_dir = project.session(&parent_session_id);
        let session_dir = parent_dir.subagent(&session.id);
        let blocks_dir = session_dir.path().join("blocks");
        let messages = load_messages_from_db(&session.id, &blocks_dir)
            .await
            .into_iter()
            .filter_map(|item| match item {
                HistoryItem::Message(message) => Some(message),
                HistoryItem::Display(_) | HistoryItem::Plan(_) | HistoryItem::Summary(_) => None,
            })
            .collect();
        subagents.push(SubagentSnapshot {
            session_id: session.id,
            parent_session_id,
            spawn_tool_use_id,
            agent_label: session
                .agent_label
                .unwrap_or_else(|| "Subagent".to_string()),
            status: SubagentStatus::Completed,
            messages,
        });
    }
    subagents
}

pub(super) async fn persist_initial_user_message(
    session_id: Option<&str>,
    session_dir: Option<&SessionDir>,
    llm_message: Option<Message>,
    start: RunStart,
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
            persist_one(session_dir, session_id, &blocks_dir, llm_message).await;
        }
        RunStart::SplitDisplayMessage { display_message } => {
            persist_split_display_message(session_dir, session_id, llm_message, display_message)
                .await;
        }
        RunStart::Continue => {}
    }
}

/// 持久化单条消息到 JSONL + SQLite。
pub(super) async fn persist_one(
    session_dir: &SessionDir,
    session_id: &str,
    blocks_dir: &Path,
    msg: Message,
) {
    let _ = session_dir.append_history(&msg);
    persist_db_only(session_id, blocks_dir, &msg).await;
}

pub(super) fn persist_llm_history_only(session_dir: &SessionDir, msg: &Message) {
    let _ = session_dir.append_history(msg);
}

async fn persist_split_display_message(
    session_dir: &SessionDir,
    session_id: &str,
    llm_msg: Message,
    display_msg: DisplayMessage,
) {
    let _ = session_dir.append_history(&llm_msg);
    let _ = db::global_db()
        .insert_display_message(session_id, &display_msg)
        .await;
}

pub(super) async fn persist_db_only(session_id: &str, blocks_dir: &Path, msg: &Message) {
    let new_msg = NewMessage {
        session_id: session_id.to_string(),
        role: msg.role.to_string(),
        blocks: msg.content.clone(),
        kind: "normal".to_string(),
        created_at: Utc::now(),
        blocks_dir: blocks_dir.to_path_buf(),
    };
    let _ = db::global_db().insert_message(&new_msg).await;
}

pub(super) async fn persist_plan_db_only(session_id: &str, plan: &DisplayPlan) {
    let _ = db::global_db().insert_plan_message(session_id, plan).await;
}

pub(super) async fn persist_compact_summary_db_only(session_id: &str, summary: &DisplaySummary) {
    let _ = db::global_db()
        .insert_compact_summary_message(session_id, summary)
        .await;
}

fn split_embedded_plan_blocks(
    blocks: Vec<ContentBlock>,
    role: Role,
    persisted_plan_markdowns: &HashSet<String>,
    message_id: i64,
    message_created_at: DateTime<Utc>,
) -> (Vec<ContentBlock>, Vec<DisplayPlan>) {
    if role != Role::Assistant {
        return (blocks, Vec::new());
    }

    let mut out_blocks = Vec::with_capacity(blocks.len());
    let mut legacy_plans = Vec::new();

    for block in blocks {
        let ContentBlock::Text(text_block) = block else {
            out_blocks.push(block);
            continue;
        };

        if let Some(markdown) = extract_proposed_plan_text(&text_block.text) {
            let markdown = normalized_plan_markdown(&markdown);
            if !markdown.is_empty() && !persisted_plan_markdowns.contains(&markdown) {
                let idx = legacy_plans.len() + 1;
                legacy_plans.push(DisplayPlan {
                    id: format!("legacy-{message_id}-{idx}"),
                    title: title_from_markdown(&markdown),
                    markdown: markdown.clone(),
                    path: std::path::PathBuf::new(),
                    created_at: message_created_at,
                });
            }
        }

        let stripped = strip_proposed_plan_blocks(&text_block.text);
        if !stripped.trim().is_empty() {
            out_blocks.push(ContentBlock::from_text(stripped));
        }
    }

    (out_blocks, legacy_plans)
}

fn normalized_plan_markdown(markdown: &str) -> String {
    markdown.trim().to_string()
}

fn title_from_markdown(markdown: &str) -> String {
    for line in markdown.lines() {
        let title = line.trim().trim_start_matches('#').trim();
        if !title.is_empty() {
            return title.chars().take(80).collect();
        }
    }
    "Plan".to_string()
}
