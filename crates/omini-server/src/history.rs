//! 从 server 持久化记录恢复 core/TUI 使用的历史视图。
//!
//! SQLite 中的消息按 kind 存储为不同 JSON 形状；这里把它们重新组装成
//! `HistoryItem`，并兼容旧版本嵌在 assistant 文本中的 proposed plan。

use crate::store::{self, Database};
use chrono::{DateTime, Utc};
use omini_core::config::project::ProjectDir;
use omini_core::types::display::{DisplayMessage, DisplayPlan, DisplaySummary, HistoryItem};
use omini_core::types::events::{SubagentSnapshot, SubagentStatus};
use omini_core::types::message::{ContentBlock, Message, Role};
use omini_core::types::proposed_plan::{extract_proposed_plan_text, strip_proposed_plan_blocks};
use std::collections::HashSet;
use std::path::Path;

/// 加载一个会话的消息历史，跳过无法解析的损坏记录以保证会话仍可打开。
pub(crate) async fn load_messages(
    db: &Database,
    session_id: &str,
    blocks_dir: &Path,
) -> Vec<HistoryItem> {
    let stored = match db.get_messages(session_id).await {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(session_id, error = %error, "failed to load messages");
            return Vec::new();
        }
    };

    // 新版计划会单独以 kind=plan 落盘；先收集 markdown，后面拆 legacy 文本块时避免重复展示。
    let persisted_plan_markdowns = stored
        .iter()
        .filter(|sm| sm.kind == "plan")
        .filter_map(|sm| serde_json::from_str::<DisplayPlan>(&sm.content).ok())
        .map(|plan| normalized_plan_markdown(&plan.markdown))
        .collect::<HashSet<_>>();

    let mut messages = Vec::with_capacity(stored.len());
    for sm in stored {
        // kind 决定这条记录恢复成哪类 HistoryItem；normal message 才继续解析 ContentBlock。
        if sm.kind == "display" {
            match serde_json::from_str::<DisplayMessage>(&sm.content) {
                Ok(display) => messages.push(HistoryItem::Display(display)),
                Err(error) => {
                    tracing::warn!(session_id, error = %error, "failed to parse display message");
                }
            }
            continue;
        }

        if sm.kind == "plan" {
            match serde_json::from_str::<DisplayPlan>(&sm.content) {
                Ok(plan) => messages.push(HistoryItem::Plan(plan)),
                Err(error) => {
                    tracing::warn!(session_id, error = %error, "failed to parse plan message");
                }
            }
            continue;
        }

        if sm.kind == "compact_summary" {
            match serde_json::from_str::<DisplaySummary>(&sm.content) {
                Ok(summary) => messages.push(HistoryItem::Summary(summary)),
                Err(error) => {
                    tracing::warn!(
                        session_id,
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
        let content_json: Vec<serde_json::Value> = match serde_json::from_str(&sm.content) {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(session_id, error = %error, "failed to parse message content");
                continue;
            }
        };
        let blocks = match store::load_blocks(&content_json, blocks_dir) {
            Ok(blocks) => blocks,
            Err(error) => {
                tracing::warn!(session_id, error = %error, "failed to load message blocks");
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
        // 旧版本把 proposed plan 混在 assistant 文本块里，加载时拆成独立 Plan 供新 UI 使用。
        messages.extend(legacy_plans.into_iter().map(HistoryItem::Plan));
    }
    messages
}

/// 加载父会话下的子 agent 历史，并恢复成已完成的 snapshot。
pub(crate) async fn load_subagents_for_session(
    db: &Database,
    session_id: &str,
    project: &ProjectDir,
) -> Vec<SubagentSnapshot> {
    let sessions = match db.list_child_sessions(session_id).await {
        Ok(sessions) => sessions,
        Err(error) => {
            tracing::warn!(session_id, error = %error, "failed to load subagents for session");
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
        // 子代理运行态不随 daemon 存活，这里从子会话历史恢复成 completed snapshot。
        let messages = load_messages(db, &session.id, &blocks_dir)
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

/// 把旧版 assistant 文本中的 `<proposed_plan>` 块拆成独立计划项。
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

    // 只对 assistant 文本块做 legacy plan 拆分，工具块和结构化内容保持原样。
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
