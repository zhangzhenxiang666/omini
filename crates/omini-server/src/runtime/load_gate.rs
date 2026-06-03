use super::*;

type RuntimeLoadWaiter = oneshot::Sender<Result<(), String>>;

/// core snapshot hydrate 的加载状态。
#[derive(Default)]
enum RuntimeLoadState {
    #[default]
    NotLoaded,
    Loading {
        waiters: Vec<RuntimeLoadWaiter>,
    },
    Loaded,
}

/// `RuntimeLoadGate` 判断调用方该直接返回、负责加载，还是等待已有加载。
pub(super) enum RuntimeLoadAction {
    AlreadyLoaded,
    Load,
    Wait(oneshot::Receiver<Result<(), String>>),
}

/// 确保同一个 session 的 core snapshot 只被一个任务加载，其他请求共享结果。
#[derive(Default)]
pub(super) struct RuntimeLoadGate {
    state: Mutex<RuntimeLoadState>,
}

impl RuntimeLoadGate {
    pub(super) fn begin_load(&self) -> RuntimeLoadAction {
        let mut loaded = self.state.lock().expect("loaded state lock poisoned");
        match &mut *loaded {
            RuntimeLoadState::Loaded => RuntimeLoadAction::AlreadyLoaded,
            RuntimeLoadState::NotLoaded => {
                *loaded = RuntimeLoadState::Loading {
                    waiters: Vec::new(),
                };
                RuntimeLoadAction::Load
            }
            RuntimeLoadState::Loading { waiters } => {
                let (tx, rx) = oneshot::channel();
                waiters.push(tx);
                RuntimeLoadAction::Wait(rx)
            }
        }
    }

    pub(super) fn finish_load(&self, result: &Result<(), CoreError>) {
        let mut loaded = self.state.lock().expect("loaded state lock poisoned");
        let waiters = match &mut *loaded {
            RuntimeLoadState::Loading { waiters } => {
                let waiters = std::mem::take(waiters);
                *loaded = if result.is_ok() {
                    RuntimeLoadState::Loaded
                } else {
                    RuntimeLoadState::NotLoaded
                };
                waiters
            }
            RuntimeLoadState::NotLoaded | RuntimeLoadState::Loaded => Vec::new(),
        };
        drop(loaded);

        let error = result
            .as_ref()
            .err()
            .map(|error| error.message().to_string());
        for waiter in waiters {
            let _ = waiter.send(match &error {
                Some(message) => Err(message.clone()),
                None => Ok(()),
            });
        }
    }

    pub(super) fn is_loaded(&self) -> bool {
        matches!(
            *self.state.lock().expect("loaded state lock poisoned"),
            RuntimeLoadState::Loaded
        )
    }
}

/// server 对单个 core 会话的适配层。
///

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn runtime_load_gate_waiters_follow_successful_loader() {
        let gate = RuntimeLoadGate::default();

        let RuntimeLoadAction::Load = gate.begin_load() else {
            panic!("first caller should load");
        };
        let RuntimeLoadAction::Wait(waiter) = gate.begin_load() else {
            panic!("second caller should wait");
        };

        gate.finish_load(&Ok(()));

        assert!(gate.is_loaded());
        assert_eq!(waiter.await.expect("waiter should receive result"), Ok(()));
        let RuntimeLoadAction::AlreadyLoaded = gate.begin_load() else {
            panic!("loaded gate should stay loaded");
        };
    }

    #[tokio::test]
    async fn runtime_load_gate_error_resets_for_retry() {
        let gate = RuntimeLoadGate::default();

        let RuntimeLoadAction::Load = gate.begin_load() else {
            panic!("first caller should load");
        };
        let RuntimeLoadAction::Wait(waiter) = gate.begin_load() else {
            panic!("second caller should wait");
        };

        let result = Err(CoreError::new("load failed"));
        gate.finish_load(&result);

        assert!(!gate.is_loaded());
        assert_eq!(
            waiter.await.expect("waiter should receive result"),
            Err("load failed".to_string())
        );
        let RuntimeLoadAction::Load = gate.begin_load() else {
            panic!("failed gate should allow retry");
        };
    }
}
