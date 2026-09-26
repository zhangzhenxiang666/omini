//! 从 server 持久化记录恢复 core/TUI 使用的历史视图。
//!
//! SQLite 中的消息按 `kind` 字段区分 JSON 形状:display / plan / compact_summary
//! 等独立形态的记录直接恢复,`kind=normal` 记录按 ContentBlock 顺序恢复成
//! `Message`。plan 与否的判定完全以 `kind` 字段为准,不再从 assistant 文本中
//! 反向解析 `<proposed_plan>` 标签。

use crate::store::{self, Database};
use omini_config::project::{ProjectDir, ThreadDir};
use omini_domain::conversation::ConversationEntry;
use omini_model::message::Role;
use omini_runtime_contract::thread_domain::{AgentTaskInfo, AgentTaskSnapshot};

/// 加载一个线程的消息历史，跳过无法解析的损坏记录以保证线程仍可打开。
pub async fn load_messages(
    db: &Database,
    thread_id: &str,
    thread_dir: &ThreadDir,
) -> Vec<ConversationEntry> {
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
        if sm.kind != "conversation_entry" {
            continue;
        }
        match serde_json::from_str::<ConversationEntry>(&content) {
            Ok(entry) => match entry {
                ConversationEntry::UserInput(input) => {
                    messages.push(ConversationEntry::UserInput(input));
                }
                ConversationEntry::AssistantMessage(output) => {
                    messages.push(ConversationEntry::AssistantMessage(output));
                }
                ConversationEntry::SystemEvent(output) => {
                    messages.push(ConversationEntry::SystemEvent(output));
                }
            },
            Err(error) => {
                tracing::warn!(thread_id, error = %error, "failed to parse conversation entry");
            }
        }
    }
    messages
}

/// 加载父线程下的子 agent 历史，并恢复成已完成的 snapshot。
pub async fn load_agent_tasks_for_thread(
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
                ConversationEntry::AssistantMessage(output) => {
                    Some(crate::conversation::model_message_from_assistant_message(
                        output,
                        Role::Assistant,
                    ))
                }
                ConversationEntry::UserInput(_) | ConversationEntry::SystemEvent(_) => None,
            })
            .collect();
        snapshots.push(AgentTaskSnapshot {
            task: AgentTaskInfo {
                task_id: task.task_id,
                thread_id: task.agent_thread_id,
                parent_run_id: task.parent_run_id,
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
