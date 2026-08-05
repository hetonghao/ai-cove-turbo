use std::{io, sync::Arc, time::Duration};

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::net::TcpStream;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};
use url::Url;

use super::integration_state::{CountsSnapshot, FixtureConfig, FixtureServer, PrivateBehavior};
use crate::proxy::{Metrics, ProxyHandle, ProxyOptions, start_proxy};

type ClientWebSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

async fn start_test_proxy(server: &FixtureServer) -> io::Result<ProxyHandle> {
    start_proxy(ProxyOptions {
        upstream: server.fixture.upstream.clone(),
        compression_enabled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        websocket_enabled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        ai_cove_private_websocket_zstd: true,
        metrics: Arc::new(Metrics::default()),
        preferred_ports: vec![0],
        max_request_body_bytes: 1024 * 1024,
    })
    .await
    .map_err(io::Error::other)
}

async fn connect_local(proxy: &ProxyHandle) -> io::Result<(ClientWebSocket, u16)> {
    let mut endpoint = Url::parse(proxy.endpoint()).map_err(io::Error::other)?;
    endpoint
        .set_scheme("ws")
        .map_err(|()| io::Error::other("invalid ws URL"))?;
    endpoint.set_path("/v1/responses");
    let (client, response) = connect_async(endpoint.as_str())
        .await
        .map_err(io::Error::other)?;
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

async fn next_close_code(client: &mut ClientWebSocket) -> io::Result<u16> {
    loop {
        let Some(message) = client.next().await else {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "local websocket closed",
            ));
        };
        match message.map_err(io::Error::other)? {
            Message::Close(frame) => {
                return Ok(frame.map_or(1000, |frame| u16::from(frame.code)));
            }
            Message::Ping(payload) => {
                client
                    .send(Message::Pong(payload))
                    .await
                    .map_err(io::Error::other)?;
            }
            Message::Text(_) | Message::Binary(_) | Message::Pong(_) | Message::Frame(_) => {}
        }
    }
}

fn assert_counts(counts: CountsSnapshot, private: usize, messages: usize, http: usize) {
    assert_eq!(counts.private_handshakes, private);
    assert_eq!(counts.private_messages, messages);
    assert_eq!(counts.http_requests, http);
}

#[tokio::test]
async fn local_101_first_turn_http_and_one_failed_prewarm() -> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::Fail,
        delay_http: false,
    })
    .await?;
    let proxy = start_test_proxy(&server).await?;
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
    assert_eq!(server.fixture.counts().await.private_handshakes, 0);
    send_create(&mut client).await?;
    server.fixture.wait_private(1).await?;
    server.fixture.wait_http(1).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    assert_counts(server.fixture.counts().await, 1, 0, 1);
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
    let proxy = start_test_proxy(&server).await?;
    let (mut client, status) =
        tokio::time::timeout(Duration::from_millis(500), connect_local(&proxy))
            .await
            .map_err(io::Error::other)??;
    assert_eq!(status, 101);
    send_create(&mut client).await?;
    server.fixture.wait_private(1).await?;
    server.fixture.wait_http(1).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    send_create(&mut client).await?;
    server.fixture.wait_http(2).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    assert_counts(server.fixture.counts().await, 1, 0, 2);
    server.fixture.release_private();
    server.fixture.wait_ready(1).await?;
    send_create(&mut client).await?;
    server.fixture.wait_messages(1).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    assert_counts(server.fixture.counts().await, 1, 1, 2);
    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn idle_1012_forces_http_and_reprewarms() -> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::IdleRestart,
        delay_http: false,
    })
    .await?;
    let proxy = start_test_proxy(&server).await?;
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
    send_create(&mut client).await?;
    server.fixture.wait_http(2).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    server.fixture.wait_ready(2).await?;
    assert_counts(server.fixture.counts().await, 2, 1, 2);
    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn active_ws_failure_closes_without_http_replay() -> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::ActiveFailure,
        delay_http: false,
    })
    .await?;
    let proxy = start_test_proxy(&server).await?;
    let (mut client, status) = connect_local(&proxy).await?;
    assert_eq!(status, 101);
    send_create(&mut client).await?;
    server.fixture.wait_ready(1).await?;
    server.fixture.wait_http(1).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    send_create(&mut client).await?;
    server.fixture.wait_messages(1).await?;
    assert_eq!(next_event_type(&mut client).await?, "error");
    assert_eq!(next_close_code(&mut client).await?, 1011);
    assert_counts(server.fixture.counts().await, 1, 1, 1);
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
    let proxy = start_test_proxy(&server).await?;
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
    server.fixture.release_private();
    send_create(&mut client).await?;
    server.fixture.wait_http(2).await?;
    server.fixture.wait_private(2).await?;
    drop(client);
    server.fixture.release_http();
    server.fixture.release_private();
    assert_counts(server.fixture.counts().await, 2, 0, 2);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}
