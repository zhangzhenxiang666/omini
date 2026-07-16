use crate::{event::bridge::thinking_display_changed_protocol_event, session::SessionRuntime};
use omini_core::CoreError;
use omini_protocol as client_proto;

impl SessionRuntime {
    pub fn set_thinking_display(
        &self,
        request: client_proto::SetThinkingDisplayRequest,
    ) -> Result<(), CoreError> {
        let mut state = self
            .project
            .load_state()
            .map_err(|error| CoreError::project_state("failed to load project state", error))?;

        let show = request.show.unwrap_or(!state.show_thinking_blocks);
        state.show_thinking_blocks = show;

        self.project
            .save_state(&state)
            .map_err(|error| CoreError::project_state("failed to save project state", error))?;

        self.broadcast_server_local_event(thinking_display_changed_protocol_event(show));
        Ok(())
    }
}
