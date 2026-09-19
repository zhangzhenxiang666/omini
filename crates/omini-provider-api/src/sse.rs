//! 自定义 SSE（Server-Sent Events）解析器。
//!
//! 将字节流按 SSE 协议解析为结构化的 [`SseEvent`]。
//! 替代外部 `eventsource-stream` crate，提供轻量可控的实现。

use bytes::{Buf, Bytes, BytesMut};
use std::fmt;
use std::pin::Pin;
use std::str::Utf8Error;
use std::task::{Context, Poll};
use tokio_stream::Stream;

/// 解析后的 SSE 事件。
#[derive(Debug, Clone, Default)]
pub struct SseEvent {
    pub event: String,
    pub data: String,
}

const MAX_BUFFER_SIZE: usize = 1024 * 1024; // 1MB 上限

/// 从传输字节流组装 SSE 事件时可能发生的错误。
///
/// 网络分块与 UTF-8 字符边界没有任何对齐保证，因此解码只能发生在完整 SSE
/// 事件形成之后。非法 UTF-8 和缓冲区溢出都属于不可恢复的响应损坏，调用方应
/// 终止当前 Provider 流并按既有策略重试，而不是继续提交部分结果。
#[derive(Debug)]
pub enum SseStreamError<E> {
    Transport(E),
    InvalidUtf8(Utf8Error),
    BufferOverflow { size: usize, max: usize },
}

impl<E: fmt::Display> fmt::Display for SseStreamError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport(error) => write!(formatter, "transport error: {error}"),
            Self::InvalidUtf8(error) => {
                write!(formatter, "SSE event is not valid UTF-8: {error}")
            }
            Self::BufferOverflow { size, max } => {
                write!(formatter, "SSE buffer size {size} exceeds limit {max}")
            }
        }
    }
}

impl<E> std::error::Error for SseStreamError<E>
where
    E: std::error::Error + 'static,
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Transport(error) => Some(error),
            Self::InvalidUtf8(error) => Some(error),
            Self::BufferOverflow { .. } => None,
        }
    }
}

pin_project_lite::pin_project! {
    /// 将字节流按 SSE 协议解析为事件流的适配器。
    pub struct SseStream<S> {
        #[pin]
        inner: S,
        buffer: BytesMut,
        terminated: bool,
    }
}

impl<S> SseStream<S> {
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            buffer: BytesMut::new(),
            terminated: false,
        }
    }
}

impl<S, E> Stream for SseStream<S>
where
    S: Stream<Item = Result<Bytes, E>>,
    E: std::fmt::Display,
{
    type Item = Result<SseEvent, SseStreamError<E>>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();

        if *this.terminated {
            return Poll::Ready(None);
        }

        loop {
            if let Some((pos, delimiter_len)) = next_event_delimiter(this.buffer) {
                let event_bytes = this.buffer.split_to(pos);
                this.buffer.advance(delimiter_len);
                let event_text = match std::str::from_utf8(&event_bytes) {
                    Ok(text) => text,
                    Err(error) => {
                        this.buffer.clear();
                        *this.terminated = true;
                        return Poll::Ready(Some(Err(SseStreamError::InvalidUtf8(error))));
                    }
                };
                let event = parse_sse_event(event_text);
                if !event.data.is_empty() {
                    return Poll::Ready(Some(Ok(event)));
                }
                continue;
            }

            match this.inner.as_mut().poll_next(cx) {
                Poll::Ready(Some(Ok(bytes))) => {
                    let next_size = this.buffer.len().saturating_add(bytes.len());
                    if next_size > MAX_BUFFER_SIZE {
                        this.buffer.clear();
                        *this.terminated = true;
                        return Poll::Ready(Some(Err(SseStreamError::BufferOverflow {
                            size: next_size,
                            max: MAX_BUFFER_SIZE,
                        })));
                    }
                    this.buffer.extend_from_slice(&bytes);
                }
                Poll::Ready(Some(Err(e))) => {
                    this.buffer.clear();
                    *this.terminated = true;
                    return Poll::Ready(Some(Err(SseStreamError::Transport(e))));
                }
                Poll::Ready(None) => {
                    *this.terminated = true;
                    if !this.buffer.is_empty() {
                        let event_bytes = this.buffer.split();
                        let event_text = match std::str::from_utf8(&event_bytes) {
                            Ok(text) => text,
                            Err(error) => {
                                return Poll::Ready(Some(Err(SseStreamError::InvalidUtf8(error))));
                            }
                        };
                        let event = parse_sse_event(event_text);
                        if !event.data.is_empty() {
                            return Poll::Ready(Some(Ok(event)));
                        }
                    }
                    return Poll::Ready(None);
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

fn next_event_delimiter(buffer: &[u8]) -> Option<(usize, usize)> {
    let lf = find_bytes(buffer, b"\n\n");
    let crlf = find_bytes(buffer, b"\r\n\r\n");
    match (lf, crlf) {
        (Some(lf), Some(crlf)) if crlf < lf => Some((crlf, 4)),
        (Some(lf), _) => Some((lf, 2)),
        (None, Some(crlf)) => Some((crlf, 4)),
        (None, None) => None,
    }
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn parse_sse_event(text: &str) -> SseEvent {
    let mut event = SseEvent::default();
    for line in text.lines() {
        if let Some(data_value) = line.strip_prefix("data:") {
            let data_value = data_value.strip_prefix(' ').unwrap_or(data_value);
            if !event.data.is_empty() {
                event.data.push('\n');
            }
            event.data.push_str(data_value);
        } else if let Some(event_value) = line.strip_prefix("event:") {
            event.event = event_value
                .strip_prefix(' ')
                .unwrap_or(event_value)
                .to_string();
        }
    }
    event
}

/// 将字节流转换为 SSE 事件流的扩展 trait。
pub trait IntoSseStream {
    fn into_sse_stream(self) -> SseStream<Self>
    where
        Self: Sized;
}

impl<S> IntoSseStream for S {
    fn into_sse_stream(self) -> SseStream<Self>
    where
        Self: Sized,
    {
        SseStream::new(self)
    }
}
