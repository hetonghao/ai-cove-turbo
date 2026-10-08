use std::{io, path::PathBuf, sync::Arc, time::Duration};

use axum::http::{HeaderValue, header};
use futures_util::{FutureExt, SinkExt, StreamExt};
use serde_json::Value;
use tokio::net::TcpStream;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};
use url::Url;

use super::integration_state::{CountsSnapshot, FixtureConfig, FixtureServer, PrivateBehavior};
use crate::proxy::{
    CapabilityTransport, Metrics, ProxyHandle, ProxyOptions, start_proxy, start_proxy_with_policy,
};

type ClientWebSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[path = "hybrid_continuation_tests.rs"]
mod continuation_tests;

#[path = "hybrid_legacy_integration_tests.rs"]
mod legacy_integration_tests;

#[path = "hybrid_observability_integration_tests.rs"]
mod observability_integration_tests;

#[path = "hybrid_session_isolation_tests.rs"]
mod session_isolation_tests;

#[path = "hybrid_transport_fallback_tests.rs"]
mod transport_fallback_tests;

async fn start_test_proxy(server: &FixtureServer) -> io::Result<(ProxyHandle, Arc<Metrics>)> {
    start_test_proxy_with_policy(server, None).await
}

async fn start_test_proxy_with_policy(
    server: &FixtureServer,
    policy_path: Option<PathBuf>,
) -> io::Result<(ProxyHandle, Arc<Metrics>)> {
    let metrics = Arc::new(Metrics::default());
    let options = ProxyOptions {
        upstream: server.fixture.upstream.clone(),
        compression_enabled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        websocket_enabled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        ai_cove_private_websocket_zstd: true,
        metrics: Arc::clone(&metrics),
        preferred_ports: vec![0],
        max_request_body_bytes: 64 * 1024 * 1024,
    };
    let proxy = match policy_path {
        Some(path) => start_proxy_with_policy(options, Some(path), None).await,
        None => start_proxy(options).await,
    }
    .map_err(io::Error::other)?;
    Ok((proxy, metrics))
}

async fn connect_local(proxy: &ProxyHandle) -> io::Result<(ClientWebSocket, u16)> {
    connect_local_with_authorization(proxy, None).await
}

async fn connect_local_with_authorization(
    proxy: &ProxyHandle,
    authorization: Option<&'static str>,
) -> io::Result<(ClientWebSocket, u16)> {
    connect_local_with_headers(proxy, authorization, None, None).await
}

async fn connect_local_with_headers(
    proxy: &ProxyHandle,
    authorization: Option<&'static str>,
    thread_id: Option<&str>,
    scope_state: Option<&'static str>,
) -> io::Result<(ClientWebSocket, u16)> {
    let mut endpoint = Url::parse(proxy.endpoint()).map_err(io::Error::other)?;
    endpoint
        .set_scheme("ws")
        .map_err(|()| io::Error::other("invalid ws URL"))?;
    endpoint.set_path("/v1/responses");
    let mut request = endpoint
        .as_str()
        .into_client_request()
        .map_err(io::Error::other)?;
    if let Some(authorization) = authorization {
        request.headers_mut().insert(
            header::AUTHORIZATION,
            HeaderValue::from_static(authorization),
        );
    }
    if let Some(thread_id) = thread_id {
        request.headers_mut().insert(
            "thread-id",
            HeaderValue::from_str(thread_id).map_err(io::Error::other)?,
        );
    }
    if let Some(scope_state) = scope_state {
        request
            .headers_mut()
            .insert("x-codex-turn-state", HeaderValue::from_static(scope_state));
    }
    let (client, response) = connect_async(request).await.map_err(io::Error::other)?;
    Ok((client, response.status().as_u16()))
}

async fn send_create(client: &mut ClientWebSocket) -> io::Result<()> {
    client
        .send(Message::Text(
            r#"{"type":"response.create","model":"test","input":"test"}"#.into(),
        ))
        .await
        .map_err(io::Error::other)
}

async fn send_transcript_create(client: &mut ClientWebSocket) -> io::Result<()> {
    let request = serde_json::json!({
        "type": "response.create",
        "model": "test",
        "instructions": "be brief",
        "input": [{"type": "message", "role": "user", "content": "画画"}],
    });
    client
        .send(Message::Text(request.to_string().into()))
        .await
        .map_err(io::Error::other)
}

async fn send_tool_output_continuation(
    client: &mut ClientWebSocket,
    previous_response_id: &str,
    call_id: &str,
) -> io::Result<()> {
    let request = serde_json::json!({
        "type": "response.create",
        "model": "test",
        "previous_response_id": previous_response_id,
        "input": [{
            "type": "custom_tool_call_output",
            "call_id": call_id,
            "output": "done",
        }],
    });
    client
        .send(Message::Text(request.to_string().into()))
        .await
        .map_err(io::Error::other)
}

async fn send_gemini_history(
    client: &mut ClientWebSocket,
    previous_response_id: Option<&str>,
) -> io::Result<()> {
    let mut request = serde_json::json!({
        "type": "response.create",
        "model": "gemini-3.8-flash",
        "input": [
            {"type": "function_call", "call_id": "call-1"},
            {"type": "message", "role": "assistant", "content": "working"},
            {"type": "function_call_output", "call_id": "call-1", "output": "ok"}
        ]
    });
    if let Some(previous_response_id) = previous_response_id {
        request["previous_response_id"] = Value::String(previous_response_id.to_owned());
    }
    client
        .send(Message::Text(request.to_string().into()))
        .await
        .map_err(io::Error::other)
}

async fn send_create_with_metadata(client: &mut ClientWebSocket) -> io::Result<()> {
    let request = serde_json::json!({
        "type": "response.create",
        "model": "gpt-5.3-codex",
        "input": "test",
        "client_metadata": {
            "session_id": "session-123",
            "thread_id": "thread-123",
            "x-codex-turn-metadata": r#"{"session_id":"session-123","thread_id":"thread-123"}"#,
        },
    });
    client
        .send(Message::Text(request.to_string().into()))
        .await
        .map_err(io::Error::other)
}

async fn send_create_with_size(client: &mut ClientWebSocket, bytes: usize) -> io::Result<()> {
    const PREFIX: &str = r#"{"type":"response.create","model":"test","input":""#;
    const SUFFIX: &str = r#""}"#;
    let input_bytes = bytes
        .checked_sub(PREFIX.len() + SUFFIX.len())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "payload size too small"))?;
    let mut payload = String::with_capacity(bytes);
    payload.push_str(PREFIX);
    payload.extend(std::iter::repeat_n('x', input_bytes));
    payload.push_str(SUFFIX);
    if payload.len() != bytes {
        return Err(io::Error::other("payload size mismatch"));
    }
    client
        .send(Message::Text(payload.into()))
        .await
        .map_err(io::Error::other)
}

async fn send_cancel(client: &mut ClientWebSocket) -> io::Result<()> {
    client
        .send(Message::Text(r#"{"type":"response.cancel"}"#.into()))
        .await
        .map_err(io::Error::other)
}

async fn next_event_type(client: &mut ClientWebSocket) -> io::Result<String> {
    let value = next_event_value(client).await?;
    value
        .get("type")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "event type missing"))
}

async fn next_event_value(client: &mut ClientWebSocket) -> io::Result<Value> {
    loop {
        let Some(message) = client.next().await else {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "local websocket closed",
            ));
        };
        let message = message.map_err(io::Error::other)?;
        let payload = match message {
            Message::Text(text) => text.as_bytes().to_vec(),
            Message::Binary(payload) => payload.to_vec(),
            Message::Ping(payload) => {
                client
                    .send(Message::Pong(payload))
                    .await
                    .map_err(io::Error::other)?;
                continue;
            }
            Message::Pong(_) | Message::Frame(_) => continue,
            Message::Close(_) => {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "local close"));
            }
        };
        return serde_json::from_slice(&payload).map_err(io::Error::other);
    }
}

async fn expect_missing_continuation_close(client: &mut ClientWebSocket) -> io::Result<()> {
    let error = next_event_value(client).await?;
    assert_eq!(error.get("type"), Some(&Value::from("error")));
    assert_eq!(
        error.pointer("/error/code"),
        Some(&Value::from("previous_response_not_found"))
    );
    let close = tokio::time::timeout(Duration::from_secs(1), client.next())
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "local websocket stayed open"))?;
    let Some(Ok(Message::Close(Some(frame)))) = close else {
        return Err(io::Error::other(
            "missing continuation did not close local websocket",
        ));
    };
    assert_eq!(u16::from(frame.code), 1002);
    Ok(())
}

fn assert_counts(counts: CountsSnapshot, private: usize, messages: usize, http: usize) {
    assert_eq!(counts.private_handshakes, private);
    assert_eq!(counts.private_messages, messages);
    assert_eq!(counts.http_requests, http);
}

fn assert_counts_with_min_private(
    counts: CountsSnapshot,
    minimum_private: usize,
    messages: usize,
    http: usize,
) {
    assert!(counts.private_handshakes >= minimum_private);
    assert_eq!(counts.private_messages, messages);
    assert_eq!(counts.http_requests, http);
}

async fn wait_websocket_handshakes(metrics: &Metrics, expected: u64) -> io::Result<()> {
    tokio::time::timeout(Duration::from_secs(3), async {
        while metrics.snapshot().websocket_handshakes < expected {
            tokio::task::yield_now().await;
        }
    })
    .await
    .map_err(|_| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            "websocket pool did not become ready",
        )
    })
}

#[tokio::test]
async fn local_101_stays_responsive_when_pool_prewarm_fails() -> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::Fail,
        delay_http: false,
    })
    .await?;
    let (proxy, _) = start_test_proxy(&server).await?;
    let (mut client, status) = connect_local(&proxy).await?;
    assert_eq!(status, 101);
    client
        .send(Message::Ping(b"probe".to_vec().into()))
        .await
        .map_err(io::Error::other)?;
    let Some(Ok(Message::Pong(_))) = client.next().await else {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "local websocket did not answer probe",
        ));
    };
    send_create(&mut client).await?;
    server.fixture.wait_http(1).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    assert_counts_with_min_private(server.fixture.counts().await, 2, 0, 1);
    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn websocket_gemini_initial_and_continuation_send_reordered_history() -> io::Result<()> {
    // Given: a ready Hybrid WebSocket receives malformed Gemini tool history.
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::Persistent,
        delay_http: false,
    })
    .await?;
    let (proxy, _) = start_test_proxy(&server).await?;
    let (mut client, status) = connect_local(&proxy).await?;
    assert_eq!(status, 101);

    // When: initial and continuation response.create frames are sent through the real flow.
    send_gemini_history(&mut client, None).await?;
    server.fixture.wait_messages(1).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    send_gemini_history(&mut client, Some("response-1")).await?;
    server.fixture.wait_messages(2).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");

    // Then: both private upstream payloads contain function output before assistant message.
    let payloads = server.fixture.private_payloads().await;
    assert_eq!(payloads.len(), 2);
    for payload in payloads {
        let value: Value = serde_json::from_slice(&payload).map_err(io::Error::other)?;
        let item_types = value
            .get("input")
            .and_then(Value::as_array)
            .ok_or_else(|| io::Error::other("upstream input missing"))?
            .iter()
            .filter_map(|item| item.get("type").and_then(Value::as_str))
            .collect::<Vec<_>>();
        assert_eq!(
            item_types,
            vec!["function_call", "function_call_output", "message"]
        );
    }

    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn idle_local_sessions_never_start_or_retry_upstream_connections() -> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::Fail,
        delay_http: false,
    })
    .await?;
    let (proxy, _) = start_test_proxy(&server).await?;
    let (client, status) = connect_local(&proxy).await?;
    assert_eq!(status, 101);
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert_counts(server.fixture.counts().await, 0, 0, 0);
    assert!(proxy.connection_snapshot().await.transitions.is_empty());
    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn late_connection_after_http_fallback_is_closed_without_refill() -> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::Delay,
        delay_http: false,
    })
    .await?;
    let (proxy, _) = start_test_proxy(&server).await?;
    let (mut client, _) = connect_local(&proxy).await?;
    send_create(&mut client).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    assert_counts(server.fixture.counts().await, 1, 0, 1);
    server.fixture.release_private();
    server.fixture.wait_normal_closes(1).await?;
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert_counts(server.fixture.counts().await, 1, 0, 1);
    assert_eq!(proxy.connection_snapshot().await.current_connections, 0);
    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn large_create_uses_http_before_ready_websocket_and_session_continues() -> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::Persistent,
        delay_http: false,
    })
    .await?;
    let (proxy, metrics) = start_test_proxy(&server).await?;
    let (mut client, status) = connect_local(&proxy).await?;
    assert_eq!(status, 101);

    send_create_with_size(&mut client, super::MAX_HYBRID_WEBSOCKET_REQUEST_BYTES - 1).await?;
    server.fixture.wait_messages(1).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");

    for (index, bytes) in [18_700_000, 23_060_000, 33_460_000].into_iter().enumerate() {
        send_create_with_size(&mut client, bytes).await?;
        server.fixture.wait_http(index + 1).await?;
        server.fixture.wait_normal_closes(index + 1).await?;
        assert_eq!(next_event_type(&mut client).await?, "response.completed");
        assert_counts_with_min_private(server.fixture.counts().await, 1, index + 1, index + 1);

        send_create(&mut client).await?;
        server.fixture.wait_messages(index + 2).await?;
        assert_eq!(next_event_type(&mut client).await?, "response.completed");
    }
    assert_counts_with_min_private(server.fixture.counts().await, 1, 4, 3);
    let routes = metrics
        .traffic_snapshot()
        .recent_requests
        .into_iter()
        .map(|event| serde_json::to_value(event).map_err(io::Error::other))
        .collect::<io::Result<Vec<_>>>()?;
    assert_eq!(
        routes
            .iter()
            .filter(|event| event.get("route") == Some(&Value::from("hybridLargeRequestHttp")))
            .count(),
        3
    );
    assert_eq!(metrics.snapshot().http_fallbacks, 0);
    assert_eq!(metrics.snapshot().hybrid_large_request_http, 3);
    assert_eq!(metrics.snapshot().hybrid_ws, 4);

    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn private_websocket_is_isolated_by_authorization() -> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::Persistent,
        delay_http: false,
    })
    .await?;
    let (proxy, _) = start_test_proxy(&server).await?;
    let (mut a, _) = connect_local_with_authorization(&proxy, Some("Bearer account-a")).await?;
    let (mut b, _) = connect_local_with_authorization(&proxy, Some("Bearer account-b")).await?;
    send_create(&mut a).await?;
    assert_eq!(next_event_type(&mut a).await?, "response.completed");
    send_create(&mut b).await?;
    assert_eq!(next_event_type(&mut b).await?, "response.completed");
    assert_counts(server.fixture.counts().await, 2, 2, 0);
    drop(a);
    drop(b);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn local_connections_open_only_one_websocket_per_request() -> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::Delay,
        delay_http: false,
    })
    .await?;
    let (proxy, metrics) = start_test_proxy(&server).await?;
    let (mut first_client, first_status) = connect_local(&proxy).await?;
    let (mut second_client, second_status) = connect_local(&proxy).await?;
    assert_eq!(first_status, 101);
    assert_eq!(second_status, 101);

    assert_counts(server.fixture.counts().await, 0, 0, 0);

    send_create(&mut first_client).await?;
    send_create(&mut second_client).await?;
    server.fixture.wait_private(2).await?;
    server.fixture.release_private_all();
    wait_websocket_handshakes(&metrics, 2).await?;
    server.fixture.wait_messages(2).await?;
    assert_eq!(
        next_event_type(&mut first_client).await?,
        "response.completed"
    );
    assert_eq!(
        next_event_type(&mut second_client).await?,
        "response.completed"
    );
    assert_counts_with_min_private(server.fixture.counts().await, 2, 2, 0);

    drop(first_client);
    drop(second_client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn released_session_connection_uses_explicit_normal_close() -> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::Persistent,
        delay_http: false,
    })
    .await?;
    let (proxy, _) = start_test_proxy(&server).await?;
    let (mut client, status) = connect_local(&proxy).await?;
    assert_eq!(status, 101);

    send_create(&mut client).await?;
    server.fixture.wait_messages(1).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    drop(client);

    server.fixture.wait_normal_closes(1).await?;
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn idle_1012_reconnects_only_for_next_request() -> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::IdleRestart,
        delay_http: false,
    })
    .await?;
    let (proxy, metrics) = start_test_proxy(&server).await?;
    let (mut client, status) = connect_local(&proxy).await?;
    assert_eq!(status, 101);
    send_create(&mut client).await?;
    server.fixture.wait_messages(1).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    server.fixture.wait_close_frames(1).await?;
    send_create(&mut client).await?;
    server.fixture.wait_messages(2).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    assert_counts_with_min_private(server.fixture.counts().await, 2, 2, 0);
    let events = serde_json::to_value(metrics.traffic_snapshot().recent_requests)
        .map_err(io::Error::other)?;
    let failure = events
        .as_array()
        .and_then(|events| {
            events
                .iter()
                .find(|event| event.get("failurePhase") == Some(&Value::from("hybridIdle")))
        })
        .ok_or_else(|| io::Error::other("hybrid idle failure was not persisted"))?;
    assert_eq!(failure.get("status"), Some(&Value::from(1012)));
    assert_eq!(failure.get("failureReason"), Some(&Value::from("restart")));
    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn request_during_idle_1012_rebuild_waits_for_websocket() -> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::IdleRestartDelayedReconnect,
        delay_http: false,
    })
    .await?;
    let (proxy, metrics) = start_test_proxy(&server).await?;
    let (mut client, status) = connect_local(&proxy).await?;
    assert_eq!(status, 101);

    send_create(&mut client).await?;
    server.fixture.wait_messages(1).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    server.fixture.wait_close_frames(1).await?;

    send_create(&mut client).await?;
    let release = server.fixture.clone();
    let route_metrics = Arc::clone(&metrics);
    let release_task = tokio::spawn(async move {
        let _ = tokio::time::timeout(Duration::from_millis(200), async {
            while route_metrics.snapshot().hybrid_recovery_http == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await;
        release.release_private();
    });
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    release_task.await.map_err(io::Error::other)?;
    assert_counts_with_min_private(server.fixture.counts().await, 2, 2, 0);

    for _ in 0..8 {
        server.fixture.release_private();
    }
    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn idle_application_frame_reconnects_on_next_request_without_closing_client() -> io::Result<()>
{
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::IdleMessage,
        delay_http: false,
    })
    .await?;
    let (proxy, metrics) = start_test_proxy(&server).await?;
    let (mut client, status) = connect_local(&proxy).await?;
    assert_eq!(status, 101);
    send_create(&mut client).await?;
    server.fixture.wait_messages(1).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    client
        .send(Message::Ping(b"still-alive".to_vec().into()))
        .await
        .map_err(io::Error::other)?;
    let Some(Ok(Message::Pong(_))) = client.next().await else {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "local websocket closed after idle upstream application frame",
        ));
    };
    send_create(&mut client).await?;
    server.fixture.wait_messages(2).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    assert!(metrics.snapshot().websocket_failures >= 1);
    let events = serde_json::to_value(metrics.traffic_snapshot().recent_requests)
        .map_err(io::Error::other)?;
    assert!(events.as_array().is_some_and(|events| {
        events
            .iter()
            .all(|event| event.get("failurePhase") != Some(&Value::from("hybridIdle")))
    }));
    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn orphan_idle_request_error_is_replaced_without_user_visible_failure() -> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::IdleError,
        delay_http: false,
    })
    .await?;
    let (proxy, metrics) = start_test_proxy(&server).await?;
    let (mut client, status) = connect_local(&proxy).await?;
    assert_eq!(status, 101);

    send_create(&mut client).await?;
    server.fixture.wait_messages(1).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    server.fixture.wait_normal_closes(1).await?;

    client
        .send(Message::Ping(b"still-alive".to_vec().into()))
        .await
        .map_err(io::Error::other)?;
    let Some(Ok(Message::Pong(_))) = client.next().await else {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "local websocket closed after an idle upstream error event",
        ));
    };
    send_create(&mut client).await?;
    server.fixture.wait_messages(2).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    assert_counts_with_min_private(server.fixture.counts().await, 2, 2, 0);

    let events = serde_json::to_value(metrics.traffic_snapshot().recent_requests)
        .map_err(io::Error::other)?;
    assert!(events.as_array().is_some_and(|events| {
        events
            .iter()
            .all(|event| event.get("failurePhase") != Some(&Value::from("hybridIdle")))
    }));

    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn idle_unexpected_eof_is_replaced_without_user_visible_failure() -> io::Result<()> {
    // Given: 一条已绑定连接会在响应完成后收到空闲 1011 unexpected EOF。
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::IdleUnexpectedEof,
        delay_http: false,
    })
    .await?;
    let (proxy, metrics) = start_test_proxy(&server).await?;
    let (mut client, status) = connect_local(&proxy).await?;
    assert_eq!(status, 101);

    // When: WebSocket 请求完成、空闲连接断开并由池自动补充。
    observability_integration_tests::send_observed_create(&mut client).await?;
    server.fixture.wait_messages(1).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    server.fixture.wait_close_frames(1).await?;
    observability_integration_tests::send_observed_create(&mut client).await?;
    server.fixture.wait_messages(2).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");

    // Then: 本地会话连续可用，请求列表不暴露内部替换，但诊断仍保留。
    assert_counts_with_min_private(server.fixture.counts().await, 2, 2, 0);
    assert!(metrics.snapshot().websocket_failures >= 1);
    let events = serde_json::to_value(metrics.traffic_snapshot().recent_requests)
        .map_err(io::Error::other)?;
    assert!(events.as_array().is_some_and(|events| {
        events
            .iter()
            .all(|event| event.get("failurePhase") != Some(&Value::from("hybridIdle")))
    }));
    let snapshot = proxy.connection_snapshot().await;
    assert!(snapshot.transitions.iter().all(|item| {
        item.thread_id.as_deref() != Some(observability_integration_tests::OBSERVED_THREAD_ID)
    }));
    assert!(
        snapshot
            .recent_closed
            .iter()
            .any(|item| item.reason.contains("1011") && item.reason.contains("unexpected EOF"))
    );

    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn active_ws_failure_closes_client_without_replay() -> io::Result<()> {
    // Given: the client has completed one request while Turbo warms a private WebSocket.
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::ActiveFailure,
        delay_http: false,
    })
    .await?;
    let (proxy, metrics) = start_test_proxy(&server).await?;
    let (mut client, status) = connect_local(&proxy).await?;
    assert_eq!(status, 101);
    // When: the warmed private WebSocket fails during the next active request.
    send_create(&mut client).await?;
    server.fixture.wait_messages(1).await?;

    // Then: Turbo terminates the failed downstream request instead of leaving it open.
    assert_eq!(next_event_type(&mut client).await?, "error");
    let close = tokio::time::timeout(Duration::from_secs(1), client.next())
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "active failure close missing"))?;
    let Some(Ok(Message::Close(Some(frame)))) = close else {
        return Err(io::Error::other("active failure close frame missing"));
    };
    assert_eq!(u16::from(frame.code), 1011);
    let event = metrics
        .traffic_snapshot()
        .recent_requests
        .into_iter()
        .find_map(|event| {
            let event = serde_json::to_value(event).ok()?;
            (event.get("route") == Some(&Value::from("hybridWs"))).then_some(event)
        })
        .ok_or_else(|| io::Error::other("active failure traffic event missing"))?;
    assert!(event.get("firstFrameMs").is_none());
    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn active_ws_1009_keeps_client_and_routes_same_retry_over_http() -> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::ActiveMessageTooBig,
        delay_http: false,
    })
    .await?;
    let (proxy, metrics) = start_test_proxy(&server).await?;
    let (mut client, status) = connect_local(&proxy).await?;
    assert_eq!(status, 101);

    send_create(&mut client).await?;
    server.fixture.wait_messages(1).await?;
    let failure = next_event_value(&mut client).await?;
    assert_eq!(failure.get("type"), Some(&Value::from("response.failed")));
    assert_eq!(
        failure.pointer("/response/error/code"),
        Some(&Value::from("context_length_exceeded"))
    );
    let event = metrics
        .traffic_snapshot()
        .recent_requests
        .into_iter()
        .find_map(|event| {
            let event = serde_json::to_value(event).ok()?;
            (event.get("failurePhase") == Some(&Value::from("hybridActive"))).then_some(event)
        })
        .ok_or_else(|| io::Error::other("1009 traffic event missing"))?;
    assert_eq!(event.get("status"), Some(&Value::from(1009)));
    assert_eq!(
        event.get("failureReason"),
        Some(&Value::from("message too big"))
    );
    assert!(event.get("firstFrameMs").is_some());

    send_create(&mut client).await?;
    server.fixture.wait_http(1).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    assert_counts_with_min_private(server.fixture.counts().await, 1, 1, 1);

    client
        .send(Message::Text(
            r#"{"type":"response.create","model":"test","input":"x"}"#.into(),
        ))
        .await
        .map_err(io::Error::other)?;
    server.fixture.wait_messages(2).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    assert_counts_with_min_private(server.fixture.counts().await, 1, 2, 1);

    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn large_http_413_keeps_client_with_context_length_failure() -> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::HttpPayloadTooLarge,
        delay_http: false,
    })
    .await?;
    let (proxy, metrics) = start_test_proxy(&server).await?;
    let (mut client, status) = connect_local(&proxy).await?;
    assert_eq!(status, 101);

    send_create_with_size(&mut client, 18_700_000).await?;
    server.fixture.wait_http(1).await?;
    let failure = next_event_value(&mut client).await?;
    assert_eq!(failure.get("type"), Some(&Value::from("response.failed")));
    assert_eq!(
        failure.pointer("/response/error/code"),
        Some(&Value::from("context_length_exceeded"))
    );
    let event = metrics
        .traffic_snapshot()
        .recent_requests
        .into_iter()
        .find_map(|event| {
            let event = serde_json::to_value(event).ok()?;
            (event.get("route") == Some(&Value::from("hybridLargeRequestHttp"))).then_some(event)
        })
        .ok_or_else(|| io::Error::other("large HTTP traffic event missing"))?;
    assert_eq!(event.get("status"), Some(&Value::from(413)));
    assert_eq!(event.get("result"), Some(&Value::from("error")));
    assert_eq!(event.get("transport"), Some(&Value::from("HTTP")));
    assert_eq!(
        event.get("failurePhase"),
        Some(&Value::from("hybridActive"))
    );
    assert_eq!(
        event.get("failureReason"),
        Some(&Value::from("HTTP upstream returned status 413"))
    );

    send_create(&mut client).await?;
    server.fixture.wait_messages(1).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    assert_counts_with_min_private(server.fixture.counts().await, 1, 1, 1);

    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn active_ws_silence_pings_and_pong_keeps_the_request_alive() -> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::HoldResponse,
        delay_http: false,
    })
    .await?;
    let (proxy, _) = start_test_proxy(&server).await?;
    let (mut client, status) = connect_local(&proxy).await?;
    assert_eq!(status, 101);
    send_create(&mut client).await?;
    server.fixture.wait_messages(1).await?;
    server.fixture.wait_active_ready(1).await?;

    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(29)).await;
    assert_eq!(server.fixture.counts().await.active_pings, 0);
    tokio::time::advance(Duration::from_secs(1) + Duration::from_millis(1)).await;
    server.fixture.wait_active_pings(1).await?;
    tokio::time::resume();
    server.fixture.wait_active_ready(2).await?;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(29)).await;
    server.fixture.release_private();

    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    assert_eq!(server.fixture.counts().await.active_pings, 1);
    assert_counts_with_min_private(server.fixture.counts().await, 1, 1, 0);
    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn websocket_request_records_model_and_codex_context() -> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::Persistent,
        delay_http: false,
    })
    .await?;
    let (proxy, metrics) = start_test_proxy(&server).await?;
    let (mut client, status) = connect_local(&proxy).await?;
    assert_eq!(status, 101);

    send_create_with_metadata(&mut client).await?;
    server.fixture.wait_messages(1).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");

    let event = metrics
        .traffic_snapshot()
        .recent_requests
        .into_iter()
        .find_map(|event| {
            let event = serde_json::to_value(event).ok()?;
            (event.get("route") == Some(&Value::from("hybridWs"))).then_some(event)
        })
        .ok_or_else(|| io::Error::other("websocket traffic event missing"))?;
    assert_eq!(event.get("model"), Some(&Value::from("gpt-5.3-codex")));
    assert_eq!(event.get("threadId"), Some(&Value::from("thread-123")));
    assert_eq!(event.get("sessionId"), Some(&Value::from("session-123")));
    assert!(event.get("firstFrameMs").is_some());
    assert!(
        event
            .get("connectionId")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty())
    );

    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn active_ws_missing_pong_closes_client_without_replaying_the_request() -> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::HoldResponseNoPong,
        delay_http: false,
    })
    .await?;
    let (proxy, metrics) = start_test_proxy(&server).await?;
    let (mut client, status) = connect_local(&proxy).await?;
    assert_eq!(status, 101);
    send_create(&mut client).await?;
    server.fixture.wait_messages(1).await?;
    server.fixture.wait_active_ready(1).await?;

    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(29)).await;
    assert_eq!(metrics.snapshot().hybrid_ws, 0);
    tokio::time::advance(Duration::from_secs(1) + Duration::from_millis(1)).await;
    server.fixture.wait_active_pings(1).await?;
    tokio::time::advance(Duration::from_secs(9)).await;
    assert_eq!(metrics.snapshot().hybrid_ws, 0);
    tokio::time::advance(Duration::from_secs(1) + Duration::from_millis(1)).await;
    for _ in 0..1_000 {
        tokio::task::yield_now().await;
    }

    let failure = next_event_value(&mut client)
        .now_or_never()
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "keepalive did not fail"))??;
    assert_eq!(failure.get("type"), Some(&Value::from("error")));
    assert_eq!(
        failure.pointer("/error/message"),
        Some(&Value::from("private websocket keepalive timed out"))
    );
    assert_eq!(metrics.snapshot().hybrid_ws, 1);
    assert_counts_with_min_private(server.fixture.counts().await, 1, 1, 0);
    let close = client
        .next()
        .now_or_never()
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "keepalive close missing"))?;
    let Some(Ok(Message::Close(Some(frame)))) = close else {
        return Err(io::Error::other("keepalive close frame missing"));
    };
    assert_eq!(u16::from(frame.code), 1011);
    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn failed_terminal_then_1012_records_one_active_failure_without_replay() -> io::Result<()> {
    // Given: New API forwards one upstream error event before its replay-required 1012 close.
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::ActiveReplayRequired,
        delay_http: false,
    })
    .await?;
    let (proxy, metrics) = start_test_proxy(&server).await?;
    let (mut client, status) = connect_local(&proxy).await?;
    assert_eq!(status, 101);

    // When: the next request receives the real failure sequence.
    send_create(&mut client).await?;
    server.fixture.wait_messages(1).await?;
    let failure_event = next_event_value(&mut client).await?;
    assert_eq!(failure_event.get("type"), Some(&Value::from("error")));
    assert_eq!(
        failure_event.pointer("/error/code"),
        Some(&Value::from("upstream_error"))
    );
    assert_eq!(
        failure_event.pointer("/error/message"),
        Some(&Value::from("upstream requires HTTP replay"))
    );
    server.fixture.wait_close_frames(1).await?;

    // Then: Turbo does not replay, keeps the local session, and records one active failure.
    assert_counts_with_min_private(server.fixture.counts().await, 1, 1, 0);
    let events = serde_json::to_value(metrics.traffic_snapshot().recent_requests)
        .map_err(io::Error::other)?;
    let hybrid_events = events
        .as_array()
        .ok_or_else(|| io::Error::other("recent requests are not an array"))?
        .iter()
        .filter(|event| event.get("route") == Some(&Value::from("hybridWs")))
        .collect::<Vec<_>>();
    assert_eq!(hybrid_events.len(), 1);
    let failure = hybrid_events
        .first()
        .ok_or_else(|| io::Error::other("hybrid failure was not persisted"))?;
    assert_eq!(failure.get("result"), Some(&Value::from("error")));
    assert_eq!(
        failure.get("failurePhase"),
        Some(&Value::from("hybridActive"))
    );
    assert_eq!(failure.get("status"), Some(&Value::from(1011)));
    assert_eq!(
        failure.get("failureReason"),
        Some(&Value::from(
            "upstream_error: upstream requires HTTP replay"
        ))
    );
    assert!(failure.get("firstFrameMs").is_some());
    send_create(&mut client).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");

    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn cancelled_terminal_reuses_the_same_healthy_websocket() -> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::CancelledTerminal,
        delay_http: false,
    })
    .await?;
    let (proxy, metrics) = start_test_proxy(&server).await?;
    let (mut client, status) = connect_local(&proxy).await?;
    assert_eq!(status, 101);

    send_create(&mut client).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.cancelled");
    send_create(&mut client).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    server.fixture.wait_messages(2).await?;
    assert_counts_with_min_private(server.fixture.counts().await, 1, 2, 0);
    let cancelled = metrics
        .traffic_snapshot()
        .recent_requests
        .into_iter()
        .filter_map(|request| serde_json::to_value(request).ok())
        .find(|request| request.get("status") == Some(&serde_json::json!(499)))
        .ok_or_else(|| io::Error::other("cancelled WebSocket request was not persisted"))?;
    assert!(cancelled.get("durationMs").is_some());

    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn concurrent_create_cancel_and_client_close_do_not_replay() -> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::Delay,
        delay_http: true,
    })
    .await?;
    let (proxy, _) = start_test_proxy(&server).await?;
    let (mut client, status) = connect_local(&proxy).await?;
    assert_eq!(status, 101);
    send_create(&mut client).await?;
    server.fixture.wait_private(1).await?;
    server.fixture.wait_http(1).await?;
    send_create(&mut client).await?;
    assert_eq!(next_event_type(&mut client).await?, "error");
    send_cancel(&mut client).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.cancelled");
    server.fixture.release_http();
    send_create(&mut client).await?;
    server.fixture.wait_http(2).await?;
    drop(client);
    server.fixture.release_http();
    for _ in 0..2 {
        server.fixture.release_private();
    }
    assert_counts(server.fixture.counts().await, 1, 0, 2);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}
