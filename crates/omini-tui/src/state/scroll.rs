use super::UiState;

impl UiState {
    /// 根据滚动速度动态调整步长
    pub fn update_scroll_step(&mut self, now: tokio::time::Instant) {
        const MIN_STEP: usize = 1;
        const MAX_STEP: usize = 10;
        const ACCEL_MS: u64 = 80; // 间隔 < 80ms → 加速
        const DECEL_MS: u64 = 250; // 间隔 > 250ms → 减速
        const RESET_MS: u64 = 800; // 间隔 > 800ms → 重置为初始值

        if let Some(last) = self.last_scroll_time {
            let elapsed = now.saturating_duration_since(last);
            let ms = elapsed.as_millis() as u64;

            if ms > RESET_MS {
                self.scroll_step = MIN_STEP;
            } else if ms < ACCEL_MS {
                self.scroll_step = (self.scroll_step + 1).min(MAX_STEP);
            } else if ms > DECEL_MS {
                self.scroll_step = (self.scroll_step / 2).max(MIN_STEP);
            }
            // 中间区间：保持当前步长
        } else {
            self.scroll_step = MIN_STEP;
        }
        self.last_scroll_time = Some(now);
    }

    pub fn scroll_up(&mut self, lines: usize) {
        self.scroll_offset = self.scroll_offset.saturating_add(lines);
        self.auto_scroll = false;
    }

    pub fn scroll_down(&mut self, lines: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(lines);
        if self.scroll_offset == 0 {
            self.auto_scroll = true;
        }
    }

    /// 滚动到消息区顶部
    pub fn scroll_to_top(&mut self) {
        self.scroll_offset = usize::MAX;
        self.auto_scroll = false;
    }

    /// 滚动到消息区底部并恢复自动滚动
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
        self.auto_scroll = true;
    }
}
