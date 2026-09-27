use crate::app::state::{AgentStatus, AppState, SelectableScreenLine, SessionState};
use crate::ui::geometry::FrameGeometry;
use std::ops::Deref;
use std::time::Duration;

/// 单帧布局结果。它只反馈屏幕坐标与视口位置，不修改运行时事实。
#[derive(Debug, Default)]
pub struct FrameState {
    pub geometry: FrameGeometry,
    pub viewport: Viewport,
    pub task_id: Option<String>,
    pub drawer_scroll_offset: usize,
}

#[derive(Debug, Default)]
pub struct Viewport {
    pub total_lines: usize,
    /// 从首个变化行开始替换选择文本，避免每帧复制整段历史。
    pub selectable_patch: Option<(usize, Vec<String>)>,
    pub message_scroll_y: usize,
    pub scroll_offset: usize,
    pub auto_scroll: bool,
}

/// 视图只借用应用和当前会话；所有可变字段均为本帧展示结果。
pub struct ViewContext<'a> {
    pub app: &'a AppState,
    pub session: &'a SessionState,
    pub geometry: FrameGeometry,
    pub viewport: Viewport,
    pub drawer_scroll_offset: usize,
}

impl<'a> ViewContext<'a> {
    pub fn new(app: &'a AppState) -> Self {
        let session = app.sessions.active();
        Self {
            app,
            session,
            geometry: FrameGeometry::default(),
            viewport: Viewport {
                total_lines: session.total_lines,
                scroll_offset: session.scroll_offset,
                auto_scroll: session.auto_scroll,
                ..Viewport::default()
            },
            drawer_scroll_offset: app.dialogs.permission.permission_scroll_offset,
        }
    }

    pub fn finish(self) -> FrameState {
        FrameState {
            geometry: self.geometry,
            viewport: self.viewport,
            task_id: self.app.sessions.active_session_task_id.clone(),
            drawer_scroll_offset: self.drawer_scroll_offset,
        }
    }

    pub fn clear_selectable_screen_lines(&mut self) {
        self.geometry.selectable_screen_lines.clear();
    }

    pub fn register_selectable_screen_line(
        &mut self,
        row: u16,
        col: u16,
        width: u16,
        text: String,
    ) {
        if width > 0 {
            self.geometry
                .selectable_screen_lines
                .push(SelectableScreenLine {
                    row,
                    col,
                    width,
                    text,
                });
        }
    }

    pub fn is_run_active(&self) -> bool {
        matches!(
            self.session.agent_status,
            AgentStatus::Working | AgentStatus::Thinking | AgentStatus::AwaitingInput
        )
    }

    pub fn current_run_elapsed(&self) -> Option<Duration> {
        self.session
            .run_timer
            .as_ref()
            .map(|timer| timer.elapsed_at(tokio::time::Instant::now()))
    }

    pub fn is_run_timer_paused(&self) -> bool {
        self.session
            .run_timer
            .as_ref()
            .is_some_and(|timer| timer.is_paused())
    }
}

impl Deref for ViewContext<'_> {
    type Target = AppState;
    fn deref(&self) -> &Self::Target {
        self.app
    }
}
