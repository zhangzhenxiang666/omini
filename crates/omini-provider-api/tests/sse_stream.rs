use bytes::Bytes;
use omini_provider_api::sse::{IntoSseStream, SseStreamError};
use std::convert::Infallible;
use tokio_stream::StreamExt;

fn byte_stream(
    chunks: impl IntoIterator<Item = impl Into<Bytes>>,
) -> impl tokio_stream::Stream<Item = Result<Bytes, Infallible>> {
    tokio_stream::iter(
        chunks
            .into_iter()
            .map(|chunk| Ok(chunk.into()))
            .collect::<Vec<_>>(),
    )
}

#[tokio::test]
async fn sse_stream_valid_delimiters_and_multiline_data_preserve_event_order() {
    let stream = byte_stream([
        "event: message_start\ndata: first\n\ndata: line one\ndata: line two\r\n\r\ndata: [DONE]\n\n",
    ])
    .into_sse_stream();

    let events = stream
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .expect("SSE events should parse");

    assert_eq!(events.len(), 3);
    assert_eq!(events[0].event, "message_start");
    assert_eq!(events[0].data, "first");
    assert_eq!(events[1].event, "");
    assert_eq!(events[1].data, "line one\nline two");
    assert_eq!(events[2].data, "[DONE]");
}

#[tokio::test]
async fn sse_stream_split_and_trailing_events_are_emitted() {
    let mut stream = byte_stream(["event: mes", "sage\ndata: hel", "lo\n\n", "data: trailing"])
        .into_sse_stream();

    let first = stream
        .next()
        .await
        .expect("split event should be emitted")
        .expect("split event should parse");
    let trailing = stream
        .next()
        .await
        .expect("trailing event should be emitted")
        .expect("trailing event should parse");

    assert_eq!(
        (first.event, first.data),
        ("message".into(), "hello".into())
    );
    assert_eq!(
        (trailing.event, trailing.data),
        ("".into(), "trailing".into())
    );
    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn sse_stream_empty_event_is_skipped_and_upstream_error_is_preserved() {
    let items = vec![
        Ok(Bytes::from("event: ping\n\ndata: useful\n\n")),
        Err("connection lost"),
    ];
    let mut stream = tokio_stream::iter(items).into_sse_stream();

    let event = stream
        .next()
        .await
        .expect("non-empty event should be emitted")
        .expect("event should parse");
    let error = stream
        .next()
        .await
        .expect("upstream error should be emitted")
        .expect_err("upstream error should not become an event");

    assert_eq!((event.event, event.data), ("".into(), "useful".into()));
    assert!(matches!(
        error,
        SseStreamError::Transport(e) if e == "connection lost"
    ));
    // 传输错误后流必须终止，不能继续产出残缺事件
    assert!(stream.next().await.is_none());
}

#[tokio::test]
async fn sse_stream_oversized_chunk_terminates_stream() {
    let oversized = Bytes::from(vec![b'x'; 1024 * 1024 + 1]);
    let mut stream =
        byte_stream([oversized, Bytes::from_static(b"data: valid\n\n")]).into_sse_stream();

    // 溢出属于不可恢复的响应损坏，必须终止而不是清空缓冲后把后续片段误当完整事件
    assert!(matches!(
        stream.next().await,
        Some(Err(SseStreamError::BufferOverflow { .. }))
    ));
    assert!(stream.next().await.is_none());
}

#[tokio::test]
// 边界：网络可在任意字节处分块；完整事件只能在事件边界统一解码 UTF-8。
async fn sse_stream_preserves_multibyte_text_at_every_chunk_boundary() {
    let raw = "data: {\"text\":\"账号规模化\"}\n\n".as_bytes();

    for split in 1..raw.len() {
        let mut stream =
            byte_stream([raw[..split].to_vec(), raw[split..].to_vec()]).into_sse_stream();
        let event = stream.next().await.unwrap().unwrap();
        assert_eq!(event.data, r#"{"text":"账号规模化"}"#, "split={split}");
        assert!(stream.next().await.is_none(), "split={split}");
    }
}

#[tokio::test]
// 回归：多字节字符的首字节被拆到下一个 chunk 时，不能再产生 replacement character。
async fn sse_stream_preserves_character_split_after_first_byte() {
    let raw = "data: 账号规模化\n\n".as_bytes();
    let character = raw
        .windows("规".len())
        .position(|window| window == "规".as_bytes())
        .unwrap();
    let mut stream = byte_stream([raw[..character + 1].to_vec(), raw[character + 1..].to_vec()])
        .into_sse_stream();

    assert_eq!(stream.next().await.unwrap().unwrap().data, "账号规模化");
}

#[tokio::test]
// 反向：完整事件含非法 UTF-8 时必须失败且终止，不能 lossy 解码后继续。
async fn sse_stream_rejects_invalid_utf8_without_replacement_characters() {
    let mut stream = byte_stream([b"data: bad \xff\n\n".to_vec()]).into_sse_stream();

    assert!(matches!(
        stream.next().await,
        Some(Err(SseStreamError::InvalidUtf8(_)))
    ));
    assert!(stream.next().await.is_none());
}
