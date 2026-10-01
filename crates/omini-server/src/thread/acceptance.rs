use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

/// server 拥有的受理任务集合；会话退出前等待所有事务与启动失败结算。
#[derive(Default)]
pub(super) struct Acceptances {
    active: Mutex<usize>,
    finished: Notify,
}

pub(super) struct AcceptanceGuard(Arc<Acceptances>);

impl Acceptances {
    pub(super) fn begin(self: &Arc<Self>) -> AcceptanceGuard {
        *self.active.lock().expect("acceptance lock poisoned") += 1;
        AcceptanceGuard(Arc::clone(self))
    }

    pub(super) async fn wait_finished(&self) {
        loop {
            let notified = self.finished.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if *self.active.lock().expect("acceptance lock poisoned") == 0 {
                return;
            }
            notified.await;
        }
    }
}

impl Drop for AcceptanceGuard {
    fn drop(&mut self) {
        let mut active = self.0.active.lock().expect("acceptance lock poisoned");
        *active = active.checked_sub(1).expect("acceptance guard underflow");
        if *active == 0 {
            self.0.finished.notify_waiters();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;

    #[tokio::test]
    async fn drain_acceptance_tasks() {
        // 给定 HTTP 端已退出但宿主持有受理任务，当关闭会话，则必须等待全部结算。
        let tasks = Arc::new(Acceptances::default());
        let first = tasks.begin();
        let second = tasks.begin();
        let waiting = tasks.wait_finished();
        tokio::pin!(waiting);
        let mut context = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(waiting.as_mut().poll(&mut context).is_pending());
        drop(first);
        assert!(waiting.as_mut().poll(&mut context).is_pending());
        drop(second);
        assert!(waiting.as_mut().poll(&mut context).is_ready());
    }
}
