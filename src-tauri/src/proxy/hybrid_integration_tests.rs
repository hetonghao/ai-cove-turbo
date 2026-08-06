use std::{io, sync::Arc, time::Duration};

use axum::http::{HeaderValue, header};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::net::TcpStream;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};
use url::Url;

use super::integration_state::{CountsSnapshot, FixtureConfig, FixtureServer, PrivateBehavior};
use crate::proxy::{Metrics, ProxyHandle, ProxyOptions, start_proxy};

type ClientWebSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[path = "hybrid_continuation_tests.rs"]
mod continuation_tests;

async fn start_test_proxy(server: &FixtureServer) -> io::Result<(ProxyHandle, Arc<Metrics>)> {
    let metrics = Arc::new(Metrics::default());
    let proxy = start_proxy(ProxyOptions {
        upstream: server.fixture.upstream.clone(),
        compression_enabled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        websocket_enabled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        ai_cove_private_websocket_zstd: true,
        metrics: Arc::clone(&metrics),
        preferred_ports: vec![0],
        max_request_body_bytes: 1024 * 1024,
    })
    .await
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
    let (client, response) = connect_async(request).await.map_err(io::Error::other)?;
    Ok((client, response.status().as_u16()))
}

async fn send_create(client: &mut ClientWebSocket) -> io::Result<()> {
    client
        .send(Message::Text(
            r#"{"type":"response.create","model":"test","input":[]}"#.into(),
        ))
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
        let value: Value = serde_json::from_slice(&payload).map_err(io::Error::other)?;
        return value
            .get("type")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "event type missing"));
    }
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
    server.fixture.wait_private(2).await?;
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
async fn delayed_prewarm_keeps_not_ready_turns_http_then_switches_to_ws() -> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::Delay,
        delay_http: false,
    })
    .await?;
    let (proxy, metrics) = start_test_proxy(&server).await?;
    let (mut client, status) =
        tokio::time::timeout(Duration::from_millis(500), connect_local(&proxy))
            .await
            .map_err(io::Error::other)??;
    assert_eq!(status, 101);
    send_create(&mut client).await?;
    server.fixture.wait_private(2).await?;
    server.fixture.wait_http(1).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    send_create(&mut client).await?;
    server.fixture.wait_http(2).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    assert_eq!(metrics.snapshot().http_fallbacks, 0);
    assert_counts(server.fixture.counts().await, 2, 0, 2);
    for _ in 0..2 {
        server.fixture.release_private();
    }
    server.fixture.wait_ready(2).await?;
    wait_websocket_handshakes(&metrics, 2).await?;
    send_create(&mut client).await?;
    server.fixture.wait_messages(1).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    assert_counts(server.fixture.counts().await, 2, 1, 2);
    let snapshot = metrics.snapshot();
    assert_eq!(snapshot.hybrid_ws, 1);
    assert_eq!(snapshot.hybrid_cold_start_http, 2);
    assert_eq!(snapshot.hybrid_recovery_http, 0);
    assert_eq!(snapshot.direct_http, 0);
    let routes = metrics
        .traffic_snapshot()
        .recent_requests
        .into_iter()
        .filter_map(|event| {
            let event = serde_json::to_value(event).ok()?;
            if event.get("result")?.as_str()? == "error" {
                return None;
            }
            event.get("route")?.as_str().map(str::to_owned)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        routes,
        ["hybridColdStartHttp", "hybridColdStartHttp", "hybridWs"]
    );
    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn ready_private_websocket_is_reused_by_next_local_connection() -> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::Delay,
        delay_http: false,
    })
    .await?;
    let (proxy, metrics) = start_test_proxy(&server).await?;
    let (mut first_client, status) = connect_local(&proxy).await?;
    assert_eq!(status, 101);

    send_create(&mut first_client).await?;
    server.fixture.wait_private(1).await?;
    server.fixture.wait_http(1).await?;
    assert_eq!(
        next_event_type(&mut first_client).await?,
        "response.completed"
    );
    server.fixture.wait_private(2).await?;
    for _ in 0..2 {
        server.fixture.release_private();
    }
    server.fixture.wait_ready(2).await?;
    wait_websocket_handshakes(&metrics, 2).await?;
    first_client
        .send(Message::Ping(b"ready".to_vec().into()))
        .await
        .map_err(io::Error::other)?;
    let Some(Ok(Message::Pong(_))) = first_client.next().await else {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "first local websocket did not answer probe",
        ));
    };
    drop(first_client);

    let (mut second_client, status) = connect_local(&proxy).await?;
    assert_eq!(status, 101);
    server.fixture.wait_private(3).await?;
    send_create(&mut second_client).await?;
    server.fixture.wait_messages(1).await?;
    assert_eq!(
        next_event_type(&mut second_client).await?,
        "response.completed"
    );
    assert_counts(server.fixture.counts().await, 3, 1, 1);

    drop(second_client);
    server.fixture.release_private();
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn ready_private_websocket_is_isolated_by_authorization() -> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::Delay,
        delay_http: false,
    })
    .await?;
    let (proxy, _) = start_test_proxy(&server).await?;
    let (mut first_client, status) =
        connect_local_with_authorization(&proxy, Some("Bearer account-a")).await?;
    assert_eq!(status, 101);

    send_create(&mut first_client).await?;
    server.fixture.wait_private(1).await?;
    server.fixture.wait_http(1).await?;
    assert_eq!(
        next_event_type(&mut first_client).await?,
        "response.completed"
    );
    server.fixture.wait_private(2).await?;
    for _ in 0..2 {
        server.fixture.release_private();
    }
    server.fixture.wait_ready(2).await?;
    first_client
        .send(Message::Ping(b"ready".to_vec().into()))
        .await
        .map_err(io::Error::other)?;
    let Some(Ok(Message::Pong(_))) = first_client.next().await else {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "first local websocket did not answer probe",
        ));
    };
    drop(first_client);

    let (mut second_client, status) =
        connect_local_with_authorization(&proxy, Some("Bearer account-b")).await?;
    assert_eq!(status, 101);
    send_create(&mut second_client).await?;
    server.fixture.wait_private(4).await?;
    server.fixture.wait_http(2).await?;
    assert_eq!(
        next_event_type(&mut second_client).await?,
        "response.completed"
    );
    assert_counts(server.fixture.counts().await, 4, 0, 2);

    drop(second_client);
    for _ in 0..2 {
        server.fixture.release_private();
    }
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn local_connections_prewarm_one_spare_private_websocket() -> io::Result<()> {
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

    server.fixture.wait_private(3).await?;
    for _ in 0..3 {
        server.fixture.release_private();
    }
    server.fixture.wait_ready(3).await?;
    wait_websocket_handshakes(&metrics, 3).await?;

    send_create(&mut first_client).await?;
    send_create(&mut second_client).await?;
    server.fixture.wait_messages(2).await?;
    assert_eq!(
        next_event_type(&mut first_client).await?,
        "response.completed"
    );
    assert_eq!(
        next_event_type(&mut second_client).await?,
        "response.completed"
    );
    assert_counts(server.fixture.counts().await, 3, 2, 0);

    drop(first_client);
    drop(second_client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn idle_1012_reprewarms_before_next_request() -> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::IdleRestart,
        delay_http: false,
    })
    .await?;
    let (proxy, metrics) = start_test_proxy(&server).await?;
    let (mut client, status) = connect_local(&proxy).await?;
    assert_eq!(status, 101);
    send_create(&mut client).await?;
    server.fixture.wait_ready(1).await?;
    server.fixture.wait_http(1).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    send_create(&mut client).await?;
    server.fixture.wait_messages(1).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    server.fixture.wait_restarts(1).await?;
    server.fixture.wait_ready(2).await?;
    send_create(&mut client).await?;
    server.fixture.wait_messages(2).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    assert_counts_with_min_private(server.fixture.counts().await, 2, 2, 1);
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
async fn active_ws_failure_keeps_client_and_rewarms_without_replay() -> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::ActiveFailure,
        delay_http: false,
    })
    .await?;
    let (proxy, metrics) = start_test_proxy(&server).await?;
    let (mut client, status) = connect_local(&proxy).await?;
    assert_eq!(status, 101);
    send_create(&mut client).await?;
    server.fixture.wait_ready(1).await?;
    server.fixture.wait_http(1).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    send_create(&mut client).await?;
    server.fixture.wait_messages(1).await?;
    assert_eq!(next_event_type(&mut client).await?, "error");
    server.fixture.wait_ready(2).await?;
    send_create(&mut client).await?;
    server.fixture.wait_messages(2).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    assert_counts_with_min_private(server.fixture.counts().await, 2, 2, 1);
    let events = serde_json::to_value(metrics.traffic_snapshot().recent_requests)
        .map_err(io::Error::other)?;
    let failure = events
        .as_array()
        .and_then(|events| {
            events
                .iter()
                .find(|event| event.get("failurePhase") == Some(&Value::from("hybridActive")))
        })
        .ok_or_else(|| io::Error::other("hybrid active failure was not persisted"))?;
    assert_eq!(failure.get("status"), Some(&Value::from(1011)));
    assert_eq!(
        failure.get("failureReason"),
        Some(&Value::from("private websocket failed while active"))
    );
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
    server.fixture.wait_private(2).await?;
    drop(client);
    server.fixture.release_http();
    for _ in 0..2 {
        server.fixture.release_private();
    }
    assert_counts(server.fixture.counts().await, 2, 0, 2);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}
