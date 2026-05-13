use crate::command::Command;
use crate::runtime::AgentRuntime;
use crate::types::events::{CommandResult, RuntimeToUiEvent};
use async_trait::async_trait;

pub struct NewCommand;

#[async_trait]
impl Command for NewCommand {
    fn name(&self) -> &'static str {
        "new"
    }
    fn aliases(&self) -> &[&'static str] {
        &["clear"]
    }
    fn description(&self) -> &'static str {
        "清空当前会话，开始新对话"
    }
    async fn execute(&self, runtime: &mut AgentRuntime, _args: &str) -> CommandResult {
        // 清空 runtime 状态
        runtime.session_id = None;
        runtime.session_dir = None;
        runtime.messages.clear();

        // 通知 UI 清空状态
        runtime
            .send_event(RuntimeToUiEvent::SessionChanged {
                session_id: None,
                messages: vec![],
            })
            .await;
        runtime
            .send_event(RuntimeToUiEvent::SessionTitleChanged { title: None })
            .await;

        CommandResult::Done
    }
}
