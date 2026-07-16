use crate::{event::tool_pause::ToolPauseResolutionStart, session::SessionRuntime};

impl SessionRuntime {
    pub async fn begin_tool_pause_resolution(
        &self,
        client_id: String,
        tool_use_id: &str,
    ) -> ToolPauseResolutionStart {
        {
            let mut pending = self
                .pending_tool_pauses
                .lock()
                .expect("pending tool pauses lock poisoned");
            // 先移除 pending，保证同一个 tool pause 只有一个请求能进入 core resolve。
            if !pending.remove(tool_use_id) {
                return ToolPauseResolutionStart::AlreadyResolved;
            }
        }

        // 权限响应来自用户操作，应把发起响应的已连接客户端提升为 controller。
        if self.takeover_controller(client_id).await.is_some() {
            ToolPauseResolutionStart::Started
        } else {
            // 如果连接状态在两步之间消失，把 pending 放回，允许仍在线的客户端继续处理。
            self.pending_tool_pauses
                .lock()
                .expect("pending tool pauses lock poisoned")
                .insert(tool_use_id.to_string());
            ToolPauseResolutionStart::ClientNotConnected
        }
    }
}
