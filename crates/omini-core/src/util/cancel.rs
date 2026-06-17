use omini_provider_api::{ApiStream, RequestError};
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::Notify;

/// 在等待 LLM invoke 完成期间响应取消信号。
///
/// 返回 `None` 表示被取消，`Some(result)` 表示 invoke 完成。
///
/// 这个函数解决了一个关键问题：当 `llm_client.invoke()` 正在等待上游 HTTP 响应时，
/// 普通的 `.await` 无法被中断。通过在 `tokio::select!` 中同时监听 invoke 结果和
/// 取消通知，可以在请求阶段（stream 开始之前）也能响应取消信号。
pub async fn invoke_or_cancel(
    invoke: impl Future<Output = Result<ApiStream, RequestError>>,
    cancelled: &AtomicBool,
    cancel_notify: &Notify,
) -> Option<Result<ApiStream, RequestError>> {
    tokio::pin!(invoke);

    loop {
        if cancelled.load(Ordering::Relaxed) {
            return None;
        }

        tokio::select! {
            result = &mut invoke => return Some(result),
            _ = cancel_notify.notified() => {
                if cancelled.load(Ordering::Relaxed) {
                    return None;
                }
            }
        }
    }
}
