//! 从持久化记录恢复 core/TUI 使用的历史视图。
//!
//! messages 表按 `kind` 区分 JSON 形态:ConversationEntry 记录恢复为
//! 会话条目,其余(kind=normal 的块消息)不进入恢复视图。plan 与否的
//! 判定完全以 `kind` 为准,不从 assistant 文本反向解析标签。

use super::Store;
use omini_config::project::{ProjectDir, ThreadDir};
use omini_domain::conversation::ConversationEntry;
use omini_entity::{MessageKind, load_ui_content};
use omini_runtime_contract::thread_domain::{
    AgentTaskExecutionMode, AgentTaskInfo, AgentTaskSnapshot,
};

/// 加载一个线程的消息历史,跳过无法解析的损坏记录以保证线程仍可打开。
pub async fn load_messages(
    db: &Store,
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
    for message in stored {
        let content = match load_ui_content(&message.content, thread_dir) {
            Ok(content) => content,
            Err(error) => {
                tracing::warn!(thread_id, error = %error, "failed to load message sidecar");
                continue;
            }
        };
        if message.kind != MessageKind::ConversationEntry {
            continue;
        }
        match serde_json::from_str::<ConversationEntry>(&content) {
            Ok(entry) => messages.push(entry),
            Err(error) => {
                tracing::warn!(thread_id, error = %error, "failed to parse conversation entry");
            }
        }
    }
    messages
}

/// 加载父线程的直接异步子任务,并恢复每个任务的完整会话历史。
pub async fn load_agent_tasks(
    db: &Store,
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
    for task in tasks.into_iter().filter(|task| {
        task.depth == 1
            && task.parent_task_id.is_none()
            && task.execution_mode == AgentTaskExecutionMode::Background
    }) {
        let thread_dir = project.thread(&task.agent_thread_id);
        let history = load_messages(db, &task.agent_thread_id, &thread_dir).await;
        let result = task
            .result_json
            .as_deref()
            .and_then(|json| serde_json::from_str(json).ok());
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
                depth: 1,
                execution_mode: AgentTaskExecutionMode::Background,
                status: task.status,
                result,
                created_at: task.created_at,
                updated_at: task.updated_at,
                completed_at: task.completed_at,
                notification_delivered: task.notification_delivered,
            },
            history,
        });
    }
    snapshots
}
