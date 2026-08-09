use omini_runtime_contract as runtime_contract;
use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
};

pub enum ToolPauseResolutionStart {
    Started,
    AlreadyResolved,
    ClientNotConnected,
}

/// runtime 事件对 pending tool pause 集合的影响。
#[derive(Debug, PartialEq, Eq)]
enum ToolPauseUpdate {
    Add(String),
    Remove(Vec<String>),
}

pub fn apply_tool_pause_update(
    pending: &Arc<Mutex<HashSet<String>>>,
    event: &runtime_contract::RuntimeToServerEvent,
) {
    let Some(update) = tool_pause_update(event) else {
        return;
    };
    let mut pending = pending.lock().expect("pending tool pauses lock poisoned");
    match update {
        ToolPauseUpdate::Add(tool_use_id) => {
            pending.insert(tool_use_id);
        }
        ToolPauseUpdate::Remove(tool_use_ids) => {
            for tool_use_id in tool_use_ids {
                pending.remove(&tool_use_id);
            }
        }
    }
}

/// 从 runtime event 提取 pending pause 的增删清空操作。
fn tool_pause_update(event: &runtime_contract::RuntimeToServerEvent) -> Option<ToolPauseUpdate> {
    match event {
        runtime_contract::RuntimeToServerEvent::ToolPauseRequested(request) => {
            Some(ToolPauseUpdate::Add(request.tool_use_id.clone()))
        }
        runtime_contract::RuntimeToServerEvent::ToolResult(result) => {
            Some(ToolPauseUpdate::Remove(vec![result.tool_use_id.clone()]))
        }
        runtime_contract::RuntimeToServerEvent::AgentTaskEvent(event) => match &event.payload {
            omini_domain::events::AgentTaskEvent::ToolResult { tool_result } => {
                let tool_use_id = &tool_result.tool_use_id;
                Some(ToolPauseUpdate::Remove(vec![format!(
                    "{}:{tool_use_id}",
                    event.thread_id
                )]))
            }
            _ => None,
        },
        runtime_contract::RuntimeToServerEvent::RunStarted => None,
        runtime_contract::RuntimeToServerEvent::RunFinished => None,
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omini_domain::events::{ToolPauseKind, ToolPauseRequest, UserInputPreview};
    use omini_domain::message::ToolResultBlock;

    #[test]
    fn tool_pause_requested_event_adds_pending_resolution_id() {
        let event = runtime_contract::RuntimeToServerEvent::ToolPauseRequested(ToolPauseRequest {
            tool_use_id: "pause_1".to_string(),
            preview_tool_use_id: None,
            tool_name: "write".to_string(),
            permission_source: None,
            source_session_id: None,
            source_agent_label: None,
            kind: ToolPauseKind::UserInput(UserInputPreview {
                questions: Vec::new(),
            }),
        });

        assert_eq!(
            tool_pause_update(&event),
            Some(ToolPauseUpdate::Add("pause_1".to_string()))
        );
    }

    #[test]
    fn tool_result_event_removes_pending_resolution_id() {
        let event = runtime_contract::RuntimeToServerEvent::ToolResult(ToolResultBlock {
            tool_use_id: "pause_1".to_string(),
            content: "done".to_string(),
            is_error: false,
            metadata: None,
        });

        assert_eq!(
            tool_pause_update(&event),
            Some(ToolPauseUpdate::Remove(vec!["pause_1".to_string()]))
        );
    }
}
