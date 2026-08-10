use std::{io, time::Duration};

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::{
    Message,
    protocol::{CloseFrame, frame::coding::CloseCode},
};

use super::{
    Case, Mode, ResponseTiming, authenticated_request, classify_hybrid_round_transport,
    collect_sample, record_application_message, reported_reconnects, sample,
    validate_hybrid_lifecycle, validate_hybrid_round_transports, websocket_close_error,
    websocket_error, websocket_round_error,
};
use crate::{
    benchmark::{
        BenchmarkResult, BenchmarkSettings, RoundTransport, benchmark_error,
        settings::WorkloadSource, websocket_payload,
    },
    proxy::MetricsSnapshot,
};

#[test]
fn records_first_application_message_and_ignores_control_frames() {
    let mut timing = ResponseTiming::default();

    assert!(!record_application_message(
        &mut timing,
        &Message::Ping(vec![1].into()),
        Duration::from_millis(2),
    ));
    assert!(record_application_message(
        &mut timing,
        &Message::Text("first".into()),
        Duration::from_millis(4),
    ));
    assert!(record_application_message(
        &mut timing,
        &Message::Binary(vec![2].into()),
        Duration::from_millis(5),
    ));

    assert_eq!(timing.response_events, 2);
    assert_eq!(timing.first_event, Some(Duration::from_millis(4)));
}

#[test]
fn standard_websocket_request_does_not_offer_turbo_or_deflate_extensions() -> BenchmarkResult<()> {
    let request = authenticated_request("ws://127.0.0.1:1/v1/responses", "test-key")
        .map_err(benchmark_error)?;

    assert!(request.headers().get("Sec-WebSocket-Extensions").is_none());
    assert!(request.headers().get("Sec-WebSocket-Protocol").is_none());
    assert_eq!(
        request
            .headers()
            .get("Authorization")
            .and_then(|value| value.to_str().ok()),
        Some("Bearer test-key")
    );
    Ok(())
}

#[test]
fn accepts_not_ready_hybrid_turns_over_http() -> BenchmarkResult<()> {
    validate_hybrid_lifecycle(2, 3, 5)?;
    Ok(())
}

#[test]
fn rejects_hybrid_lifecycle_without_http_or_with_missing_messages() {
    assert!(validate_hybrid_lifecycle(0, 5, 5).is_err());
    assert!(validate_hybrid_lifecycle(2, 2, 5).is_err());
}

#[test]
fn records_hybrid_round_transport_and_requires_http_first() -> BenchmarkResult<()> {
    let before = MetricsSnapshot::default();
    let mut after_http = before;
    after_http.requests = 1;
    let mut after_websocket = before;
    after_websocket.websocket_messages = 1;

    assert_eq!(
        classify_hybrid_round_transport(before, after_http)?,
        RoundTransport::Http
    );
    assert_eq!(
        classify_hybrid_round_transport(before, after_websocket)?,
        RoundTransport::WebSocket
    );
    assert!(classify_hybrid_round_transport(before, before).is_err());
    let mut after_both = after_http;
    after_both.websocket_messages = 1;
    assert!(classify_hybrid_round_transport(before, after_both).is_err());
    validate_hybrid_round_transports(&[
        RoundTransport::Http,
        RoundTransport::WebSocket,
        RoundTransport::Http,
    ])?;
    assert!(
        validate_hybrid_round_transports(&[RoundTransport::WebSocket, RoundTransport::Http])
            .is_err()
    );
    Ok(())
}

#[test]
fn excludes_hybrid_pool_prewarm_handshakes_from_reconnects() {
    let before = MetricsSnapshot::default();
    let mut after = before;
    after.websocket_handshakes = 7;
    after.websocket_active = 7;

    assert_eq!(reported_reconnects(super::Mode::Hybrid, before, after), 0);

    after.hybrid_recovery_http = 1;
    assert_eq!(reported_reconnects(super::Mode::Hybrid, before, after), 1);
}

#[test]
fn reports_hybrid_pool_connection_churn_as_reconnect() {
    // Given
    let before = MetricsSnapshot::default();
    let mut after = before;
    after.websocket_handshakes = 8;
    after.websocket_active = 7;

    // When
    let reconnects = reported_reconnects(super::Mode::Hybrid, before, after);

    // Then
    assert_eq!(reconnects, 1);
}

#[test]
fn reports_websocket_close_code_reason_path_and_round() {
    let close = CloseFrame {
        code: CloseCode::from(1012),
        reason: "service restart".into(),
    };

    let close_error = websocket_close_error(Some(&close));
    let error = websocket_round_error(super::Mode::PrivateWebSocket, 4, &close_error);
    let message = error.to_string();

    assert!(message.contains("Turbo WS + 自适应 zstd round 4"));
    assert!(message.contains("1012"));
    assert!(message.contains("service restart"));
    assert_eq!(close_error.kind(), io::ErrorKind::ConnectionAborted);
}

#[test]
fn does_not_retry_protocol_websocket_closes() {
    let close = CloseFrame {
        code: CloseCode::Protocol,
        reason: "invalid frame".into(),
    };

    let error = websocket_close_error(Some(&close));

    assert_eq!(error.kind(), io::ErrorKind::Other);
}

#[test]
fn retries_proxy_reported_upstream_websocket_errors() {
    let close = CloseFrame {
        code: CloseCode::Protocol,
        reason: "upstream websocket error".into(),
    };

    let error = websocket_close_error(Some(&close));

    assert_eq!(error.kind(), io::ErrorKind::ConnectionAborted);
}

#[test]
fn retries_transient_websocket_handshake_statuses_only() -> BenchmarkResult<()> {
    for (status, expected) in [
        (429, io::ErrorKind::ConnectionAborted),
        (502, io::ErrorKind::ConnectionAborted),
        (400, io::ErrorKind::Other),
    ] {
        let response = tokio_tungstenite::tungstenite::http::Response::builder()
            .status(status)
            .body(None)
            .map_err(benchmark_error)?;
        let error = websocket_error(tokio_tungstenite::tungstenite::Error::Http(Box::new(
            response,
        )));
        assert_eq!(error.kind(), expected);
    }
    Ok(())
}

#[tokio::test]
async fn fails_fast_when_websocket_receives_failure_terminal() -> BenchmarkResult<()> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = tokio_tungstenite::accept_async(stream)
            .await
            .map_err(websocket_error)?;
        let _ = socket.next().await;
        socket
            .send(Message::Text(r#"{"type":"response.failed"}"#.into()))
            .await
            .map_err(websocket_error)?;
        let _ = socket.next().await;
        Ok::<(), io::Error>(())
    });
    let payloads = [websocket_payload("test-model", "test")];
    let settings = BenchmarkSettings {
        upstream: format!("http://{address}"),
        model: "test-model".to_owned(),
        prompt: "test-prompt".to_owned(),
        workload_source: WorkloadSource::BuiltIn,
        runs: 4,
        warmups: 0,
        timeout: Duration::from_millis(100),
    };
    let url = format!("ws://{address}/v1/responses");

    let result = sample(
        &Case {
            url: &url,
            authorization: "test-key",
            payloads: &payloads,
            metrics: None,
            mode: Mode::Hybrid,
        },
        &settings,
        MetricsSnapshot::default(),
    )
    .await;
    server.abort();
    let Err(error) = result else {
        return Err(io::Error::other(
            "WebSocket failure terminal was admitted as a benchmark sample",
        ));
    };

    assert_eq!(error.kind(), io::ErrorKind::ConnectionAborted);
    assert!(error.to_string().contains("response.failed"));
    Ok(())
}

#[tokio::test]
async fn chains_each_websocket_round_to_the_previous_response() -> BenchmarkResult<()> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let address = listener.local_addr()?;
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut socket = tokio_tungstenite::accept_async(stream)
            .await
            .map_err(websocket_error)?;
        let mut requests = Vec::new();
        for round in 1..=3 {
            let message = socket
                .next()
                .await
                .ok_or_else(|| io::Error::other("benchmark client closed early"))?
                .map_err(websocket_error)?;
            let Message::Text(text) = message else {
                return Err(io::Error::other("benchmark request was not text"));
            };
            requests.push(serde_json::from_str::<serde_json::Value>(&text)?);
            socket
                .send(Message::Text(
                    serde_json::json!({
                        "type": "response.completed",
                        "response": {"id": format!("resp-{round}")},
                    })
                    .to_string()
                    .into(),
                ))
                .await
                .map_err(websocket_error)?;
        }
        let _ = socket.next().await;
        Ok::<_, io::Error>(requests)
    });
    let payloads = [
        websocket_payload("test-model", "first"),
        websocket_payload("test-model", "second"),
        websocket_payload("test-model", "third"),
    ];
    let settings = BenchmarkSettings {
        upstream: format!("http://{address}"),
        model: "test-model".to_owned(),
        prompt: "test-prompt".to_owned(),
        workload_source: WorkloadSource::BuiltIn,
        runs: 4,
        warmups: 0,
        timeout: Duration::from_secs(1),
    };
    let url = format!("ws://{address}/v1/responses");

    let sample = collect_sample(
        &Case {
            url: &url,
            authorization: "test-key",
            payloads: &payloads,
            metrics: None,
            mode: Mode::PrivateWebSocket,
        },
        &settings,
    )
    .await?;
    let template_bytes =
        u64::try_from(payloads.iter().map(String::len).sum::<usize>()).map_err(benchmark_error)?;
    let requests = server.await.map_err(benchmark_error)??;
    let first = requests
        .first()
        .ok_or_else(|| io::Error::other("missing first request"))?;
    let second = requests
        .get(1)
        .ok_or_else(|| io::Error::other("missing second request"))?;
    let third = requests
        .get(2)
        .ok_or_else(|| io::Error::other("missing third request"))?;

    assert!(sample.raw_bytes > template_bytes);
    assert!(first.get("previous_response_id").is_none());
    assert_eq!(
        second
            .get("previous_response_id")
            .and_then(serde_json::Value::as_str),
        Some("resp-1")
    );
    assert_eq!(
        third
            .get("previous_response_id")
            .and_then(serde_json::Value::as_str),
        Some("resp-2")
    );
    Ok(())
}
