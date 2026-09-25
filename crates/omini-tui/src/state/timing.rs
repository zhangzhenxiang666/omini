use std::time::Duration;
use tokio::time::Instant;

#[derive(Debug, Clone)]
pub struct RunTimer {
    started_at: Instant,
    paused_total: Duration,
    pause_started_at: Option<Instant>,
}

impl RunTimer {
    pub(super) fn started_at(started_at: Instant) -> Self {
        Self {
            started_at,
            paused_total: Duration::ZERO,
            pause_started_at: None,
        }
    }

    pub(super) fn started_with_elapsed_at(now: Instant, elapsed: Duration, paused: bool) -> Self {
        Self {
            started_at: now.checked_sub(elapsed).unwrap_or(now),
            paused_total: Duration::ZERO,
            pause_started_at: paused.then_some(now),
        }
    }

    pub(super) fn pause_at(&mut self, now: Instant) {
        if self.pause_started_at.is_none() {
            self.pause_started_at = Some(now);
        }
    }

    pub(super) fn resume_at(&mut self, now: Instant) {
        let Some(paused_at) = self.pause_started_at.take() else {
            return;
        };
        self.paused_total += now.saturating_duration_since(paused_at);
    }

    pub(super) fn elapsed_at(&self, now: Instant) -> Duration {
        let active_pause = self
            .pause_started_at
            .map(|paused_at| now.saturating_duration_since(paused_at))
            .unwrap_or(Duration::ZERO);
        now.saturating_duration_since(self.started_at)
            .saturating_sub(self.paused_total + active_pause)
    }

    pub(super) fn finish_at(mut self, now: Instant) -> Duration {
        self.resume_at(now);
        self.elapsed_at(now)
    }

    pub fn is_paused(&self) -> bool {
        self.pause_started_at.is_some()
    }
}

pub(crate) fn format_run_duration(duration: Duration) -> String {
    let total = duration.as_secs();
    let seconds = total % 60;
    let minutes = (total / 60) % 60;
    let hours = total / 3600;

    if hours > 0 {
        format!("{hours}h{minutes:02}m{seconds:02}s")
    } else if minutes > 0 {
        format!("{minutes}m{seconds:02}s")
    } else {
        format!("{seconds}s")
    }
}
