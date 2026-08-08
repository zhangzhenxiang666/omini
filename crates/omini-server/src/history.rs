//! 从 server 持久化记录恢复 core/TUI 使用的历史视图。
//!
//! SQLite 中的消息按 `kind` 字段区分 JSON 形状:display / plan / compact_summary
//! 等独立形态的记录直接恢复,`kind=normal` 记录按 ContentBlock 顺序恢复成
//! `Message`。plan 与否的判定完全以 `kind` 字段为准,不再从 assistant 文本中
//! 反向解析 `<proposed_plan>` 标签。

use crate::store::{self, Database};
use omini_config::project::{ProjectDir, ThreadDir};
use omini_domain::display::{DisplayMessage, DisplayPlan, DisplaySummary, HistoryItem};
use omini_domain::events::{SubagentSnapshot, SubagentStatus};
use omini_domain::message::{Message, Role};

/// 加载一个会话的消息历史，跳过无法解析的损坏记录以保证会话仍可打开。
pub(crate) async fn load_messages(
    db: &Database,
    thread_id: &str,
    thread_dir: &ThreadDir,
) -> Vec<HistoryItem> {
    let stored = match db.get_messages(thread_id).await {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(thread_id, error = %error, "failed to load messages");
            return Vec::new();
        }
    };

    let mut messages = Vec::with_capacity(stored.len());
    for sm in stored {
        let content = match store::load_ui_content(&sm.content, thread_dir) {
            Ok(content) => content,
            Err(error) => {
                tracing::warn!(thread_id, error = %error, "failed to load message sidecar");
                continue;
            }
        };
        // kind 决定这条记录恢复成哪类 HistoryItem；normal message 才继续解析 ContentBlock。
        if sm.kind == "display" {
            match serde_json::from_str::<DisplayMessage>(&content) {
                Ok(display) => messages.push(HistoryItem::Display(display)),
                Err(error) => {
                    tracing::warn!(thread_id, error = %error, "failed to parse display message");
                }
            }
            continue;
        }

        if sm.kind == "plan" {
            match serde_json::from_str::<DisplayPlan>(&content) {
                Ok(plan) => messages.push(HistoryItem::Plan(plan)),
                Err(error) => {
                    tracing::warn!(thread_id, error = %error, "failed to parse plan message");
                }
            }
            continue;
        }

        if sm.kind == "compact_summary" {
            match serde_json::from_str::<DisplaySummary>(&content) {
                Ok(summary) => messages.push(HistoryItem::Summary(summary)),
                Err(error) => {
                    tracing::warn!(
                        thread_id,
                        error = %error,
                        "failed to parse compact summary message"
                    );
                }
            }
            continue;
        }

        let role = match sm.role.as_str() {
            "user" => Role::User,
            "assistant" => Role::Assistant,
            _ => continue,
        };
        let content_json: Vec<serde_json::Value> = match serde_json::from_str(&content) {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(thread_id, error = %error, "failed to parse message content");
                continue;
            }
        };
        let blocks = match store::load_blocks(&content_json, thread_dir) {
            Ok(blocks) => blocks,
            Err(error) => {
                tracing::warn!(thread_id, error = %error, "failed to load message blocks");
                continue;
            }
        };
        if !blocks.is_empty() {
            messages.push(HistoryItem::Message(Message::new(role, blocks)));
        }
    }
    messages
}

/// 加载父会话下的子 agent 历史，并恢复成已完成的 snapshot。
pub(crate) async fn load_subagents_for_thread(
    db: &Database,
    thread_id: &str,
    project: &ProjectDir,
) -> Vec<SubagentSnapshot> {
    let sessions = match db.list_child_threads(thread_id).await {
        Ok(sessions) => sessions,
        Err(error) => {
            tracing::warn!(thread_id, error = %error, "failed to load subagents for thread");
            return Vec::new();
        }
    };

    let mut subagents = Vec::with_capacity(sessions.len());
    for session in sessions {
        let Some(parent_session_id) = session.parent_thread_id.clone() else {
            continue;
        };
        let Some(spawn_tool_use_id) = session.spawn_tool_use_id.clone() else {
            continue;
        };
        let thread_dir = project.thread(&session.id);
        // 子代理运行态不随 daemon 存活，这里从子会话历史恢复成 completed snapshot。
        let messages = load_messages(db, &session.id, &thread_dir)
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
