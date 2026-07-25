use crate::error::RuntimeError;
use crate::tools::{PendingToolPause, PendingToolPauses};
use omini_domain::events::ToolPauseResponse;
use std::sync::Arc;

/// 负责匹配并解除工具执行中的权限或用户输入暂停。
#[derive(Clone)]
pub struct ToolPauseResolver {
    pending_tool_pauses: PendingToolPauses,
}

impl ToolPauseResolver {
    pub fn new(pending_tool_pauses: PendingToolPauses) -> Self {
        Self {
            pending_tool_pauses,
        }
    }

    pub fn pending_tool_pauses(&self) -> PendingToolPauses {
        Arc::clone(&self.pending_tool_pauses)
    }

    pub fn resolve_tool_pause(
        &self,
        tool_use_id: &str,
        response: ToolPauseResponse,
    ) -> Result<(), RuntimeError> {
        let waiter = self
            .pending_tool_pauses
            .lock()
            .expect("pending tool pause mutex poisoned")
            .remove(tool_use_id);

        match (waiter, response) {
            (
                Some(PendingToolPause::Permission(tx)),
                response @ ToolPauseResponse::Permission { .. },
            )
            | (Some(PendingToolPause::Permission(tx)), response @ ToolPauseResponse::Cancelled) => {
                tx.send(response)
                    .map_err(|_| RuntimeError::ToolPauseWaiterClosed {
                        tool_use_id: tool_use_id.to_string(),
                    })
            }
            (
                Some(PendingToolPause::UserInput(tx)),
                response @ ToolPauseResponse::UserInput { .. },
            )
            | (Some(PendingToolPause::UserInput(tx)), response @ ToolPauseResponse::Cancelled) => {
                tx.send(response)
                    .map_err(|_| RuntimeError::ToolPauseWaiterClosed {
                        tool_use_id: tool_use_id.to_string(),
                    })
            }
            (Some(_), _) => Err(RuntimeError::ToolPauseResponseTypeMismatch {
                tool_use_id: tool_use_id.to_string(),
            }),
            // 已结束或重复响应保持幂等。
            (None, _) => Ok(()),
        }
    }

    pub fn drain_pending_tool_pauses(&self) {
        let waiters = self
            .pending_tool_pauses
            .lock()
            .expect("pending tool pause mutex poisoned")
            .drain()
            .map(|(_, waiter)| waiter)
            .collect::<Vec<_>>();

        for waiter in waiters {
            match waiter {
                PendingToolPause::Permission(tx) | PendingToolPause::UserInput(tx) => {
                    let _ = tx.send(ToolPauseResponse::Cancelled);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[test]
    fn resolving_unknown_tool_pause_is_idempotent() {
        let resolver = ToolPauseResolver::new(Arc::new(Mutex::new(HashMap::new())));

        let result = resolver.resolve_tool_pause(
            "toolu_done",
            ToolPauseResponse::Permission {
                approved: false,
                note: None,
            },
        );

        assert!(result.is_ok());
    }
}
