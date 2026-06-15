use super::*;

pub(crate) enum ToolPauseResolutionStart {
    Started,
    AlreadyResolved,
    ClientNotConnected,
}

/// runtime 事件对 pending tool pause 集合的影响。
#[derive(Debug, PartialEq, Eq)]
enum ToolPauseUpdate {
    Add(String),
    Remove(Vec<String>),
    Clear,
}

pub(super) fn apply_tool_pause_update(
    pending: &Arc<Mutex<HashSet<String>>>,
    event: &RuntimeToServerEvent,
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
        ToolPauseUpdate::Clear => pending.clear(),
    }
}

/// 从 runtime event 提取 pending pause 的增删清空操作。
fn tool_pause_update(event: &RuntimeToServerEvent) -> Option<ToolPauseUpdate> {
    match event {
        RuntimeToServerEvent::ToolPauseRequested(request) => {
            Some(ToolPauseUpdate::Add(request.tool_use_id.clone()))
        }
        RuntimeToServerEvent::ToolResult(result) => {
            Some(ToolPauseUpdate::Remove(vec![result.tool_use_id.clone()]))
        }
        RuntimeToServerEvent::SubagentToolResult(event) => {
            let session_id = &event.session_id;
            let tool_use_id = &event.tool_result.tool_use_id;
            // 子代理暂停在 UI 中可能用 session_id:tool_use_id 表示，两个 key 都要清掉。
            Some(ToolPauseUpdate::Remove(vec![
                tool_use_id.clone(),
                format!("{session_id}:{tool_use_id}"),
            ]))
        }
        RuntimeToServerEvent::RunStarted | RuntimeToServerEvent::RunFinished => {
            Some(ToolPauseUpdate::Clear)
        }
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
        let event = RuntimeToServerEvent::ToolPauseRequested(ToolPauseRequest {
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
        let event = RuntimeToServerEvent::ToolResult(ToolResultBlock {
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
