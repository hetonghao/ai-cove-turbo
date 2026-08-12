use axum::http::{HeaderMap, HeaderValue, Uri, header};
use tokio_tungstenite::tungstenite::{Message, protocol::frame::coding::CloseCode};

use super::{
    flow, http,
    sse::{
        HttpFallback, SseParser, http_request_payload, idle_event_diagnostic,
        is_internal_idle_request_error, is_terminal_event,
    },
};

#[test]
fn parses_split_crlf_comments_and_multiple_data_lines() {
    let mut parser = SseParser::default();

    parser.push(b": comment\r\ndata: first\r\n\r\ndata: sec");
    parser.push(b"ond\r\ndata: line\r\n\r\n");

    assert_eq!(
        parser.finish(),
        vec![b"first".to_vec(), b"second\nline".to_vec()]
    );
}

#[test]
fn recognizes_done_and_terminal_response_events() {
    assert!(is_terminal_event(b"[DONE]"));
    assert!(is_terminal_event(br#"{"type":"response.completed"}"#));
    assert!(is_terminal_event(br#"{"type":"response.failed"}"#));
    assert!(is_terminal_event(br#"{"type":"response.incomplete"}"#));
    assert!(is_terminal_event(br#"{"type":"response.cancelled"}"#));
    assert!(is_terminal_event(br#"{"type":"error"}"#));
    assert!(!is_terminal_event(
        br#"{"type":"response.output_text.delta"}"#
    ));
}

#[test]
fn idle_error_diagnostic_keeps_safe_error_code_without_message() {
    let diagnostic = idle_event_diagnostic(
        br#"{"type":"error","error":{"code":"invalid_request","message":"must-not-persist"}}"#,
        None,
    );

    assert_eq!(
        diagnostic,
        "空闲上游 WebSocket 收到意外二进制消息；解码=成功；事件=error；响应ID=缺失；错误码=invalid_request"
    );
    assert!(!diagnostic.contains("must-not-persist"));
}

#[test]
fn internal_idle_request_error_requires_exact_orphan_signature() {
    assert!(is_internal_idle_request_error(
        br#"{"type":"error","error":{"code":"do_request_failed"}}"#
    ));
    for payload in [
        br#"{"type":"error","response":{"id":"response-1"},"error":{"code":"do_request_failed"}}"#
            .as_slice(),
        br#"{"type":"error","response_id":"response-1","error":{"code":"do_request_failed"}}"#
            .as_slice(),
        br#"{"type":"error","error":{"code":"invalid_request"}}"#.as_slice(),
        br#"{"type":"response.failed","error":{"code":"do_request_failed"}}"#.as_slice(),
    ] {
        assert!(!is_internal_idle_request_error(payload));
    }
}

#[test]
fn converts_response_create_into_streaming_http_payload() -> std::io::Result<()> {
    let prepared = http_request_payload(br#"{"type":"response.create","model":"test","input":[]}"#)
        .map_err(std::io::Error::other)?;
    let HttpFallback::Request(payload) = prepared.fallback else {
        return Err(std::io::Error::other(
            "HTTP request unexpectedly requires WS",
        ));
    };
    let value: serde_json::Value =
        serde_json::from_slice(&payload).map_err(std::io::Error::other)?;

    assert_eq!(value.get("stream"), Some(&serde_json::Value::Bool(true)));
    assert!(value.get("type").is_none());
    assert_eq!(
        value.get("model").and_then(serde_json::Value::as_str),
        Some("test")
    );
    Ok(())
}

#[test]
fn classifies_supported_response_request_sources() -> std::io::Result<()> {
    // Given: every upstream-supported source and the source-free boundary shapes.
    let provided = [
        br#"{"type":"response.create","input":[]}"#.as_slice(),
        br#"{"type":"response.create","previous_response_id":"response-1"}"#.as_slice(),
        br#"{"type":"response.create","prompt":{}}"#.as_slice(),
        br#"{"type":"response.create","conversation":"conversation-1"}"#.as_slice(),
    ];
    let missing = [
        br#"{"type":"response.create"}"#.as_slice(),
        br#"{"type":"response.create","input":null,"prompt":null,"conversation":null}"#.as_slice(),
        br#"{"type":"response.create","previous_response_id":""}"#.as_slice(),
        br#"{"type":"response.create","previous_response_id":7}"#.as_slice(),
    ];

    // When: the response.create boundary parses each payload.
    for payload in provided {
        let prepared = http_request_payload(payload).map_err(std::io::Error::other)?;

        // Then: every supported source is accepted independently.
        assert!(prepared.has_request_source);
    }
    for payload in missing {
        let prepared = http_request_payload(payload).map_err(std::io::Error::other)?;

        // Then: absent, null, empty, and malformed continuation sources stay source-free.
        assert!(!prepared.has_request_source);
    }
    Ok(())
}

#[test]
fn reads_canonical_thread_id_from_response_create() -> std::io::Result<()> {
    let request = serde_json::json!({
        "type": "response.create",
        "client_metadata": {
            "thread_id": "flat-thread",
            "x-codex-turn-metadata": r#"{"thread_id":"canonical-thread"}"#,
        },
    });
    let prepared =
        http_request_payload(request.to_string().as_bytes()).map_err(std::io::Error::other)?;

    assert_eq!(prepared.thread_id.as_deref(), Some("canonical-thread"));
    Ok(())
}

#[tokio::test]
async fn does_not_forward_done_sentinel_but_forwards_json_terminal() {
    let (events, mut received) = tokio::sync::mpsc::channel(4);
    let mut parser = SseParser::default();
    parser.push(b"data: [DONE]\n\n");
    assert!(
        super::http::send_sse_events(&mut parser, &events)
            .await
            .is_ok_and(|terminal| terminal)
    );
    assert!(matches!(
        received.recv().await,
        Some(super::WorkerEvent::Terminal {
            upstream: None,
            response_id: None,
        })
    ));
    assert!(received.try_recv().is_err());

    let (events, mut received) = tokio::sync::mpsc::channel(4);
    let mut parser = SseParser::default();
    parser.push(b"data: {\"type\":\"response.completed\"}\n\n");
    assert!(
        super::http::send_sse_events(&mut parser, &events)
            .await
            .is_ok_and(|terminal| terminal)
    );
    assert!(matches!(
        received.recv().await,
        Some(super::WorkerEvent::Message(
            tokio_tungstenite::tungstenite::Message::Text(_)
        ))
    ));
    assert!(matches!(
        received.recv().await,
        Some(super::WorkerEvent::Terminal {
            upstream: None,
            response_id: None,
        })
    ));
}

#[tokio::test]
async fn idle_1012_wins_over_queued_response_create() {
    let selection = flow::select_idle(
        std::future::ready(Some(Ok(Message::Text(
            r#"{"type":"response.create"}"#.into(),
        )))),
        std::future::ready(Some(Ok(Message::Close(Some(
            tokio_tungstenite::tungstenite::protocol::CloseFrame {
                code: CloseCode::from(1012),
                reason: "restart".into(),
            },
        ))))),
        std::future::pending(),
    )
    .await;

    let selected_idle_restart = match selection {
        flow::IdleSelection::Ready(Some(Ok(Message::Close(Some(frame))))) => {
            u16::from(frame.code) == 1012
        }
        _ => false,
    };
    assert!(selected_idle_restart);
}

#[tokio::test]
async fn idle_keepalive_wins_over_queued_response_create() {
    let selection = flow::select_idle(
        std::future::ready(Some(Ok(Message::Text(
            r#"{"type":"response.create"}"#.into(),
        )))),
        std::future::pending(),
        std::future::ready(()),
    )
    .await;

    assert!(matches!(selection, flow::IdleSelection::Keepalive));
}

#[test]
fn http_request_strips_websocket_handshake_headers() {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_static("Bearer test-only"),
    );
    headers.insert(
        header::SEC_WEBSOCKET_KEY,
        HeaderValue::from_static("stale-key"),
    );
    headers.insert(
        header::SEC_WEBSOCKET_VERSION,
        HeaderValue::from_static("13"),
    );
    headers.insert(
        header::SEC_WEBSOCKET_PROTOCOL,
        HeaderValue::from_static("ai-cove-zstd.v1"),
    );
    headers.insert(
        header::SEC_WEBSOCKET_EXTENSIONS,
        HeaderValue::from_static("permessage-deflate"),
    );
    let request = http::build_http_request(headers, Uri::from_static("/v1/responses"), Vec::new());

    assert_eq!(
        request.headers().get(header::AUTHORIZATION),
        Some(&HeaderValue::from_static("Bearer test-only"))
    );
    for name in [
        header::SEC_WEBSOCKET_KEY,
        header::SEC_WEBSOCKET_VERSION,
        header::SEC_WEBSOCKET_PROTOCOL,
        header::SEC_WEBSOCKET_EXTENSIONS,
    ] {
        assert!(!request.headers().contains_key(name));
    }
}
