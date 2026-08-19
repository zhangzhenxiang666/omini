use bytes::Bytes;
use omini_provider_api::sse::IntoSseStream;
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
    assert_eq!(error, "connection lost");
}

#[tokio::test]
async fn sse_stream_oversized_unterminated_chunk_is_discarded_before_next_event() {
    let oversized = Bytes::from(vec![b'x'; 1024 * 1024 + 1]);
    let mut stream =
        byte_stream([oversized, Bytes::from_static(b"data: valid\n\n")]).into_sse_stream();

    let event = stream
        .next()
        .await
        .expect("valid event should survive overflow recovery")
        .expect("valid event should parse");

    assert_eq!((event.event, event.data), ("".into(), "valid".into()));
    assert!(stream.next().await.is_none());
}
