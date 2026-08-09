use super::QueryResult;
use omini_provider_api::FinishReason;
use std::collections::HashMap;

pub const REPEAT_LIMIT: usize = 5;

/// 跨 Turn 保留的 Query 控制状态。
#[derive(Debug)]
pub struct QueryState {
    turns: usize,
    finish_reason: FinishReason,
    finalization: Option<FinalizationReason>,
}

impl QueryState {
    pub fn new() -> Self {
        Self {
            turns: 0,
            finish_reason: FinishReason::Stop,
            finalization: None,
        }
    }

    pub fn turns(&self) -> usize {
        self.turns
    }

    pub fn turn_limit_reached(&self, limit: Option<usize>) -> bool {
        self.finalization.is_none() && limit.is_some_and(|limit| self.turns >= limit)
    }

    pub fn finalize(&mut self, reason: FinalizationReason) {
        self.finalization.get_or_insert(reason);
    }

    pub fn turn_mode(&self) -> TurnMode {
        self.finalization
            .map_or(TurnMode::Normal, TurnMode::Finalization)
    }

    pub fn record_turn(&mut self, finish_reason: FinishReason) {
        self.turns += 1;
        self.finish_reason = finish_reason;
    }

    pub fn mark_cancelled(&mut self) {
        self.finish_reason = FinishReason::Error("Cancelled".to_string());
    }

    pub fn into_result(self) -> QueryResult {
        QueryResult {
            turns: self.turns,
            finish_reason: self.finish_reason,
            follow_up: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinalizationReason {
    MaxTurnsReached,
    RepeatedToolCall,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnMode {
    Normal,
    Finalization(FinalizationReason),
}

impl TurnMode {
    pub fn is_finalization(self) -> bool {
        matches!(self, Self::Finalization(_))
    }
}

/// 单轮执行结果
pub enum TurnOutcome {
    Completed {
        finish_reason: FinishReason,
        requested_finalization: Option<FinalizationReason>,
        stop_after_permission_denial: bool,
    },
    Interrupted {
        finish_reason: FinishReason,
    },
}

impl TurnOutcome {
    pub fn finish_reason(&self) -> &FinishReason {
        match self {
            Self::Completed { finish_reason, .. } | Self::Interrupted { finish_reason } => {
                finish_reason
            }
        }
    }
}

/// 监测连续相同的工具调用。
#[derive(Debug)]
pub struct RepeatGuard {
    limit: usize,
    last_call: Option<ToolCallKey>,
    repeats: usize,
}

impl RepeatGuard {
    pub fn new(limit: usize) -> Self {
        assert!(limit > 0, "repeat limit must be greater than zero");
        Self {
            limit,
            last_call: None,
            repeats: 0,
        }
    }

    /// 记录一次工具调用；返回 true 表示已经达到连续重复上限。
    pub fn observe(
        &mut self,
        tool_name: &str,
        arguments: &HashMap<String, serde_json::Value>,
    ) -> bool {
        let call = ToolCallKey::new(tool_name, arguments);
        if self.last_call.as_ref().is_some_and(|last| last == &call) {
            self.repeats += 1;
        } else {
            self.last_call = Some(call);
            self.repeats = 1;
        }
        self.repeats >= self.limit
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ToolCallKey {
    tool_name: Box<str>,
    canonical_arguments: Box<[u8]>,
}

impl ToolCallKey {
    fn new(tool_name: &str, arguments: &HashMap<String, serde_json::Value>) -> Self {
        let object: serde_json::Map<String, serde_json::Value> = arguments
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        let mut value = serde_json::Value::Object(object);
        value.sort_all_objects();

        Self {
            tool_name: tool_name.into(),
            canonical_arguments: serde_json::to_vec(&value)
                .expect("serializing serde_json::Value should not fail")
                .into_boxed_slice(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_state_records_turn_count_and_last_finish_reason() {
        let mut state = QueryState::new();
        state.record_turn(FinishReason::ToolUse);
        state.record_turn(FinishReason::Stop);

        let result = state.into_result();
        assert_eq!(result.turns, 2);
        assert!(matches!(result.finish_reason, FinishReason::Stop));
    }

    #[test]
    fn finalization_mode_is_explicit() {
        let mode = TurnMode::Finalization(FinalizationReason::MaxTurnsReached);

        assert!(mode.is_finalization());
    }

    #[test]
    fn repeat_guard_canonicalizes_argument_order() {
        let mut guard = RepeatGuard::new(2);
        let first = HashMap::from([
            ("b".to_string(), serde_json::json!(2)),
            ("a".to_string(), serde_json::json!(1)),
        ]);
        let second = HashMap::from([
            ("a".to_string(), serde_json::json!(1)),
            ("b".to_string(), serde_json::json!(2)),
        ]);

        assert!(!guard.observe("bash", &first));
        assert!(guard.observe("bash", &second));
    }
}
