use crate::{
    event::{bridge::runtime_event_from_runtime_contract_event, replay::SequencedRuntimeEvent},
    thread::ThreadRuntime,
};
use omini_core::CoreError;
use omini_protocol as client_proto;
use omini_runtime_contract as runtime_contract;
use tokio::sync::broadcast;

impl ThreadRuntime {
    pub fn subscribe(&self) -> broadcast::Receiver<SequencedRuntimeEvent> {
        self.runtime_event_tx.subscribe()
    }

    pub async fn replay_events(&self) -> Vec<SequencedRuntimeEvent> {
        self.replay_buffer
            .lock()
            .expect("replay buffer lock poisoned")
            .replay()
    }

    pub fn subscribe_controller(&self) -> broadcast::Receiver<Option<String>> {
        self.controller_tx.subscribe()
    }

    pub(super) fn broadcast_server_local_event(&self, event: client_proto::RuntimeEvent) {
        let _ = self.server_event_inbox_tx.send(event);
    }

    pub fn broadcast_agent_management_updated(
        &self,
        records: Vec<omini_domain::subagents::AgentRecord>,
    ) -> Result<(), CoreError> {
        let event = runtime_event_from_runtime_contract_event(
            runtime_contract::events::RuntimeToServerEvent::AgentManagementUpdated { records },
        )?;
        self.broadcast_server_local_event(event);
        Ok(())
    }

    /// 「在新会话中执行计划」审批通过后，server 端 fork 出新 ThreadRuntime，
    /// 通过此方法向老 session 的 ws 广播 `SessionSwitched`,TUI 收到后断开旧
    /// ws 并连接到新 session 的 ws。
    pub fn broadcast_session_switched(&self, from: String, to: String) {
        let event = match runtime_event_from_runtime_contract_event(
            runtime_contract::RuntimeToServerEvent::SessionSwitched { from, to },
        ) {
            Ok(event) => event,
            Err(error) => {
                tracing::error!(error = %error, "failed to encode session switched event");
                return;
            }
        };
        self.broadcast_server_local_event(event);
    }
}
