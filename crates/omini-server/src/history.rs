//! 从 server 持久化记录恢复 core/TUI 使用的历史视图。
//!
//! SQLite 中的消息按 `kind` 字段区分 JSON 形状:display / plan / compact_summary
//! 等独立形态的记录直接恢复,`kind=normal` 记录按 ContentBlock 顺序恢复成
//! `Message`。plan 与否的判定完全以 `kind` 字段为准,不再从 assistant 文本中
//! 反向解析 `<proposed_plan>` 标签。

use crate::store::{self, Database};
use omini_config::project::{ProjectDir, ThreadDir};
use omini_domain::display::{
    AgentTaskNotification, DisplayMessage, DisplayPlan, DisplaySummary, HistoryItem,
};
use omini_domain::events::{AgentTaskInfo, AgentTaskSnapshot};
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

        if sm.kind == "agent_task_notification" {
            match serde_json::from_str::<AgentTaskNotification>(&content) {
                Ok(notification) => messages.push(HistoryItem::AgentTaskNotification(notification)),
                Err(error) => {
                    tracing::warn!(thread_id, error = %error, "failed to parse agent task notification");
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
pub(crate) async fn load_agent_tasks_for_thread(
    db: &Database,
    thread_id: &str,
    project: &ProjectDir,
) -> Vec<AgentTaskSnapshot> {
    let tasks = match db.list_agent_tasks(thread_id).await {
        Ok(tasks) => tasks,
        Err(error) => {
            tracing::warn!(thread_id, error = %error, "failed to load agent tasks for thread");
            return Vec::new();
        }
    };

    let mut snapshots = Vec::with_capacity(tasks.len());
    for task in tasks {
        let thread_dir = project.thread(&task.agent_thread_id);
        let messages = load_messages(db, &task.agent_thread_id, &thread_dir)
            .await
            .into_iter()
            .filter_map(|item| match item {
                HistoryItem::Message(message) => Some(message),
                HistoryItem::Display(_)
                | HistoryItem::Plan(_)
                | HistoryItem::Summary(_)
                | HistoryItem::AgentTaskNotification(_) => None,
            })
            .collect();
        snapshots.push(AgentTaskSnapshot {
            task: AgentTaskInfo {
                task_id: task.task_id,
                thread_id: task.agent_thread_id,
                parent_task_id: task.parent_task_id,
                owner_thread_id: task.owner_thread_id,
                parent_thread_id: task.parent_thread_id,
                spawn_tool_use_id: task.spawn_tool_use_id,
                agent: task.agent_name,
                title: task.title,
                depth: task.depth,
                execution_mode: task.execution_mode,
                status: task.status,
                result: task.result,
                created_at: task.created_at,
                updated_at: task.updated_at,
                completed_at: task.completed_at,
                notification_delivered: task.notification_delivered,
            },
            messages,
        });
    }
    snapshots
}
