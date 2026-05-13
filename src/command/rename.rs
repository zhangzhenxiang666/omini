use crate::command::Command;
use crate::db;
use crate::runtime::AgentRuntime;
use crate::types::events::{CommandResult, RuntimeToUiEvent};
use async_trait::async_trait;

pub struct RenameCommand;

#[async_trait]
impl Command for RenameCommand {
    fn name(&self) -> &'static str {
        "rename"
    }
    fn aliases(&self) -> &[&'static str] {
        &[]
    }
    fn description(&self) -> &'static str {
        "重命名当前会话"
    }
    fn has_args(&self) -> bool {
        true
    }
    fn args_description(&self) -> Option<&'static str> {
        Some("<name>")
    }
    async fn execute(&self, runtime: &mut AgentRuntime, args: &str) -> CommandResult {
        let session_id = match &runtime.session_id {
            Some(id) => id.clone(),
            None => {
                return CommandResult::Error("当前没有激活的会话，无法重命名".to_string());
            }
        };

        let title = args.trim();
        if title.is_empty() {
            return CommandResult::Error("请提供新名称，用法: /rename <新名称>".to_string());
        }

        // 限制标题长度
        let title = title.chars().take(300).collect::<String>();

        if let Err(e) = db::global_db()
            .update_session_title(&session_id, &title)
            .await
        {
            return CommandResult::Error(format!("重命名失败: {e}"));
        }

        runtime
            .send_event(RuntimeToUiEvent::SessionTitleChanged { title: Some(title) })
            .await;

        CommandResult::Done
    }
}
