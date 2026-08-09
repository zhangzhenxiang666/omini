use super::*;

pub(super) async fn record_total_usage_and_notify(
    thread_id: &str,
    usage: Usage,
    event_tx: &mpsc::Sender<RuntimeToServerEvent>,
    persistence_tx: &mpsc::Sender<RuntimePersistenceEvent>,
    usage_state: &Arc<Mutex<SessionUsageSnapshot>>,
) {
    let _ = persistence_tx
        .send(RuntimePersistenceEvent::RecordThreadTotalUsage {
            thread_id: thread_id.to_string(),
            usage,
        })
        .await;
    let snapshot = record_total_usage_snapshot(usage_state, usage, None);
    let _ = event_tx
        .send(RuntimeToServerEvent::UsageTotalsChanged {
            total_tokens: snapshot.total_tokens,
            total_cached_tokens: snapshot.total_cached_tokens,
        })
        .await;
}

pub(super) fn record_usage_snapshot(
    usage_state: &Arc<Mutex<SessionUsageSnapshot>>,
    usage: Usage,
    context_window: Option<u32>,
) -> SessionUsageSnapshot {
    let mut snapshot = usage_state.lock().expect("thread usage lock poisoned");
    let total_tokens = usage_tokens_i64(usage);
    let cached_tokens = usage_usize_to_i64(usage.cached_tokens);
    snapshot.current_context_tokens = total_tokens;
    snapshot.total_tokens = snapshot.total_tokens.saturating_add(total_tokens);
    snapshot.total_cached_tokens = snapshot.total_cached_tokens.saturating_add(cached_tokens);
    snapshot.context_window = context_window;
    *snapshot
}

pub(super) fn record_total_usage_snapshot(
    usage_state: &Arc<Mutex<SessionUsageSnapshot>>,
    usage: Usage,
    context_window: Option<u32>,
) -> SessionUsageSnapshot {
    let mut snapshot = usage_state.lock().expect("thread usage lock poisoned");
    let total_tokens = usage_tokens_i64(usage);
    let cached_tokens = usage_usize_to_i64(usage.cached_tokens);
    snapshot.total_tokens = snapshot.total_tokens.saturating_add(total_tokens);
    snapshot.total_cached_tokens = snapshot.total_cached_tokens.saturating_add(cached_tokens);
    if context_window.is_some() {
        snapshot.context_window = context_window;
    }
    *snapshot
}

fn usage_tokens_i64(usage: Usage) -> i64 {
    usage_usize_to_i64(usage.total_tokens())
}

fn usage_usize_to_i64(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn concurrent_total_updates_preserve_main_context_usage() {
        let usage_state = Arc::new(Mutex::new(SessionUsageSnapshot {
            current_context_tokens: 77,
            ..SessionUsageSnapshot::default()
        }));
        let usage = Usage {
            prompt_tokens: 1,
            completion_tokens: 1,
            cached_tokens: 1,
        };
        let workers = (0..4)
            .map(|_| {
                let usage_state = Arc::clone(&usage_state);
                thread::spawn(move || {
                    for _ in 0..100 {
                        record_total_usage_snapshot(&usage_state, usage, None);
                    }
                })
            })
            .collect::<Vec<_>>();

        for worker in workers {
            worker.join().unwrap();
        }

        let snapshot = *usage_state.lock().expect("thread usage lock poisoned");
        assert_eq!(snapshot.current_context_tokens, 77);
        assert_eq!(snapshot.total_tokens, 800);
        assert_eq!(snapshot.total_cached_tokens, 400);
    }
}
