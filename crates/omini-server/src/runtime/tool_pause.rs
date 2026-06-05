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

pub(super) fn apply_tool_pause_update(pending: &Arc<Mutex<HashSet<String>>>, event: &RuntimeEvent) {
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

/// 从 runtime payload 提取 pending pause 的增删清空操作。
fn tool_pause_update(event: &RuntimeEvent) -> Option<ToolPauseUpdate> {
    match event
        .payload
        .get("type")
        .and_then(serde_json::Value::as_str)?
    {
        "tool_pause_requested" => event
            .payload
            .get("tool_use_id")
            .and_then(serde_json::Value::as_str)
            .map(|tool_use_id| ToolPauseUpdate::Add(tool_use_id.to_string())),
        "tool_result" => event
            .payload
            .get("tool_use_id")
            .and_then(serde_json::Value::as_str)
            .map(|tool_use_id| ToolPauseUpdate::Remove(vec![tool_use_id.to_string()])),
        "subagent_tool_result" => {
            let session_id = event
                .payload
                .get("session_id")
                .and_then(serde_json::Value::as_str)?;
            let tool_use_id = event
                .payload
                .get("tool_result")
                .and_then(|tool_result| tool_result.get("tool_use_id"))
                .and_then(serde_json::Value::as_str)?;
            // 子代理暂停在 UI 中可能用 session_id:tool_use_id 表示，两个 key 都要清掉。
            Some(ToolPauseUpdate::Remove(vec![
                tool_use_id.to_string(),
                format!("{session_id}:{tool_use_id}"),
            ]))
        }
        "run_started" | "run_finished" => Some(ToolPauseUpdate::Clear),
        kind if is_session_snapshot_kind(kind) => Some(ToolPauseUpdate::Clear),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_pause_requested_event_adds_pending_resolution_id() {
        let event = RuntimeEvent::new(
            "tool_pause_requested",
            serde_json::json!({
                "type": "tool_pause_requested",
                "tool_use_id": "pause_1",
                "tool_name": "write",
            }),
        );

        assert_eq!(
            tool_pause_update(&event),
            Some(ToolPauseUpdate::Add("pause_1".to_string()))
        );
    }

    #[test]
    fn tool_result_event_removes_pending_resolution_id() {
        let event = RuntimeEvent::new(
            "tool_result",
            serde_json::json!({
                "type": "tool_result",
                "tool_use_id": "pause_1",
                "content": "done",
                "is_error": false,
            }),
        );

        assert_eq!(
            tool_pause_update(&event),
            Some(ToolPauseUpdate::Remove(vec!["pause_1".to_string()]))
        );
    }
}
