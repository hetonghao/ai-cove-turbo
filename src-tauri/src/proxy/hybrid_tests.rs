use std::future;

use axum::http::{HeaderMap, HeaderValue, Uri, header};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{
        Message,
        protocol::{Role, frame::coding::CloseCode},
    },
};

use super::{
    flow, http,
    sse::{SseParser, http_request_payload, is_terminal_event},
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
fn converts_response_create_into_streaming_http_payload() -> std::io::Result<()> {
    let payload = http_request_payload(br#"{"type":"response.create","model":"test","input":[]}"#)
        .map_err(std::io::Error::other)?;
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
        Some(super::WorkerEvent::Terminal(None))
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
        Some(super::WorkerEvent::Terminal(None))
    ));
}

#[tokio::test]
async fn completed_prewarm_wins_over_queued_response_create() -> std::io::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let address = listener.local_addr()?;
    let client_stream = TcpStream::connect(address).await?;
    let (_server_stream, _) = listener.accept().await?;
    let upstream =
        WebSocketStream::from_raw_socket(MaybeTlsStream::Plain(client_stream), Role::Client, None)
            .await;
    let selection = flow::select_idle(
        future::ready(Some(Ok(Message::Text(
            r#"{"type":"response.create"}"#.into(),
        )))),
        future::ready(flow::PrewarmSelection::Ready(Box::new(upstream))),
        future::pending(),
        true,
        false,
    )
    .await;

    assert!(matches!(
        selection,
        flow::IdleSelection::Prewarm(flow::PrewarmSelection::Ready(_))
    ));
    Ok(())
}

#[tokio::test]
async fn idle_1012_wins_over_queued_response_create() {
    let selection = flow::select_idle(
        future::ready(Some(Ok(Message::Text(
            r#"{"type":"response.create"}"#.into(),
        )))),
        future::pending(),
        future::ready(Some(Ok(Message::Close(Some(
            tokio_tungstenite::tungstenite::protocol::CloseFrame {
                code: CloseCode::from(1012),
                reason: "restart".into(),
            },
        ))))),
        false,
        true,
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
