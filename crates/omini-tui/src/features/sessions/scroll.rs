use crate::app::state::AppState;

impl AppState {
    /// 根据滚动速度动态调整步长
    pub fn update_scroll_step(&mut self, now: tokio::time::Instant) {
        const MIN_STEP: usize = 1;
        const MAX_STEP: usize = 10;
        const ACCEL_MS: u64 = 80; // 间隔 < 80ms → 加速
        const DECEL_MS: u64 = 250; // 间隔 > 250ms → 减速
        const RESET_MS: u64 = 800; // 间隔 > 800ms → 重置为初始值

        if let Some(last) = self.selection.last_scroll_time {
            let elapsed = now.saturating_duration_since(last);
            let ms = elapsed.as_millis() as u64;

            if ms > RESET_MS {
                self.selection.scroll_step = MIN_STEP;
            } else if ms < ACCEL_MS {
                self.selection.scroll_step = (self.selection.scroll_step + 1).min(MAX_STEP);
            } else if ms > DECEL_MS {
                self.selection.scroll_step = (self.selection.scroll_step / 2).max(MIN_STEP);
            }
            // 中间区间：保持当前步长
        } else {
            self.selection.scroll_step = MIN_STEP;
        }
        self.selection.last_scroll_time = Some(now);
    }

    pub fn scroll_up(&mut self, lines: usize) {
        if let Some(view) = self
            .sessions
            .active_session_task_id
            .as_ref()
            .and_then(|task_id| self.sessions.views.get_mut(task_id))
        {
            view.scroll_offset = view.scroll_offset.saturating_add(lines);
            view.auto_scroll = false;
        } else {
            self.sessions.views["main"].scroll_offset = self.sessions.views["main"]
                .scroll_offset
                .saturating_add(lines);
            self.sessions.views["main"].auto_scroll = false;
        }
    }

    pub fn scroll_down(&mut self, lines: usize) {
        if let Some(view) = self
            .sessions
            .active_session_task_id
            .as_ref()
            .and_then(|task_id| self.sessions.views.get_mut(task_id))
        {
            view.scroll_offset = view.scroll_offset.saturating_sub(lines);
            if view.scroll_offset == 0 {
                view.auto_scroll = true;
            }
        } else {
            self.sessions.views["main"].scroll_offset = self.sessions.views["main"]
                .scroll_offset
                .saturating_sub(lines);
            if self.sessions.views["main"].scroll_offset == 0 {
                self.sessions.views["main"].auto_scroll = true;
            }
        }
    }

    /// 滚动到消息区顶部
    pub fn scroll_to_top(&mut self) {
        if let Some(view) = self
            .sessions
            .active_session_task_id
            .as_ref()
            .and_then(|task_id| self.sessions.views.get_mut(task_id))
        {
            view.scroll_offset = usize::MAX;
            view.auto_scroll = false;
        } else {
            self.sessions.views["main"].scroll_offset = usize::MAX;
            self.sessions.views["main"].auto_scroll = false;
        }
    }

    /// 滚动到消息区底部并恢复自动滚动
    pub fn scroll_to_bottom(&mut self) {
        if let Some(view) = self
            .sessions
            .active_session_task_id
            .as_ref()
            .and_then(|task_id| self.sessions.views.get_mut(task_id))
        {
            view.scroll_offset = 0;
            view.auto_scroll = true;
        } else {
            self.sessions.views["main"].scroll_offset = 0;
            self.sessions.views["main"].auto_scroll = true;
        }
    }
}
