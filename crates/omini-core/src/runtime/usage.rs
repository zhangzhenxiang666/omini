use super::*;

pub(super) async fn record_total_usage_and_notify(
    session_id: &str,
    usage: Usage,
    event_tx: &mpsc::Sender<RuntimeToUiEvent>,
    persistence_tx: &mpsc::Sender<RuntimePersistenceEvent>,
    usage_state: &Arc<Mutex<SessionUsageSnapshot>>,
) {
    let _ = persistence_tx
        .send(RuntimePersistenceEvent::RecordSessionTotalUsage {
            session_id: session_id.to_string(),
            usage,
        })
        .await;
    let snapshot = record_total_usage_snapshot(usage_state, usage, None).await;
    let _ = event_tx
        .send(RuntimeToUiEvent::UsageTotalsChanged {
            total_tokens: snapshot.total_tokens,
            total_cached_tokens: snapshot.total_cached_tokens,
        })
        .await;
}

pub(super) async fn record_usage_snapshot(
    usage_state: &Arc<Mutex<SessionUsageSnapshot>>,
    usage: Usage,
    context_window: Option<u32>,
) -> SessionUsageSnapshot {
    let mut snapshot = usage_state.lock().await;
    let total_tokens = usage_tokens_i64(usage);
    let cached_tokens = usage_usize_to_i64(usage.cached_tokens);
    snapshot.current_context_tokens = total_tokens;
    snapshot.total_tokens = snapshot.total_tokens.saturating_add(total_tokens);
    snapshot.total_cached_tokens = snapshot.total_cached_tokens.saturating_add(cached_tokens);
    snapshot.context_window = context_window;
    *snapshot
}

pub(super) async fn record_total_usage_snapshot(
    usage_state: &Arc<Mutex<SessionUsageSnapshot>>,
    usage: Usage,
    context_window: Option<u32>,
) -> SessionUsageSnapshot {
    let mut snapshot = usage_state.lock().await;
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
