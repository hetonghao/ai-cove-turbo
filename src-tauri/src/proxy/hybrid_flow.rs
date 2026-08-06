use std::{future::Future, time::Duration};

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::{Error as WebSocketError, Message};

use crate::proxy::{HttpTraffic, traffic::TrafficRoute};

use super::{
    Active, ClientWebSocket, Session,
    common::{close_client, event_type, send_error},
    http, legacy,
    sse::{HttpFallback, http_request_payload},
    websocket,
};

pub(super) enum IdleSelection {
    Client(Option<Result<Message, WebSocketError>>),
    Ready(Option<Result<Message, WebSocketError>>),
    Keepalive,
}

pub(super) async fn select_idle<Client, Ready, Keepalive>(
    client: Client,
    ready: Ready,
    keepalive: Keepalive,
    ready_enabled: bool,
) -> IdleSelection
where
    Client: Future<Output = Option<Result<Message, WebSocketError>>>,
    Ready: Future<Output = Option<Result<Message, WebSocketError>>>,
    Keepalive: Future<Output = ()>,
{
    tokio::select! {
        biased;
        () = keepalive, if ready_enabled => IdleSelection::Keepalive,
        result = ready, if ready_enabled => IdleSelection::Ready(result),
        message = client => IdleSelection::Client(message),
    }
}

pub(super) async fn poll_ready(
    ready: &mut Option<super::PrivateWebSocket>,
) -> Option<Result<Message, WebSocketError>> {
    let ready = ready.as_mut()?;
    ready.next().await
}

pub(super) async fn handle_idle_client_message(
    client: &mut ClientWebSocket,
    session: &mut Session,
    active: &mut Option<Active>,
    message: Option<Result<Message, WebSocketError>>,
) -> bool {
    let Some(message) = message else {
        return false;
    };
    let Ok(message) = message else {
        return false;
    };
    match message {
        Message::Ping(payload) => client.send(Message::Pong(payload)).await.is_ok(),
        Message::Pong(_) => true,
        Message::Close(_) => false,
        Message::Frame(_) => {
            let _ = close_client(client, 1002, "raw websocket frame is invalid").await;
            false
        }
        Message::Text(text) => {
            start_response(client, session, active, text.as_bytes().to_vec(), false).await
        }
        Message::Binary(payload) => {
            start_response(client, session, active, payload.to_vec(), true).await
        }
    }
}

async fn start_response(
    client: &mut ClientWebSocket,
    session: &mut Session,
    active: &mut Option<Active>,
    payload: Vec<u8>,
    original_binary: bool,
) -> bool {
    let Ok(event_type) = event_type(&payload) else {
        return legacy::start_legacy_response(client, session, payload, original_binary).await;
    };
    if event_type != "response.create" {
        return legacy::start_legacy_response(client, session, payload, original_binary).await;
    }

    let Ok(fallback) = http_request_payload(&payload) else {
        let _ = send_error(
            client,
            "invalid_request",
            "response.create must be a JSON object",
        )
        .await;
        return true;
    };
    if session.ready.is_none() {
        session.ready = session
            .state
            .hybrid_pool
            .checkout(&session.pool_scope)
            .await;
    }
    if session.ready.is_none() && matches!(&fallback, HttpFallback::WebSocketRequired) {
        session.ready = session
            .state
            .hybrid_pool
            .checkout_wait(&session.pool_scope, Duration::from_secs(2))
            .await;
    }
    if let Some(upstream) = session.ready.take() {
        session
            .state
            .metrics
            .record_request_route(TrafficRoute::HybridWs);
        *active = Some(websocket::start_websocket_worker(
            upstream,
            payload,
            original_binary,
            std::sync::Arc::clone(&session.state.metrics),
            session.path.clone(),
        ));
        return true;
    }

    let traffic = if session
        .state
        .hybrid_pool
        .has_initialized(&session.pool_scope)
        .await
    {
        HttpTraffic::HYBRID_RECOVERY
    } else {
        HttpTraffic::HYBRID_COLD_START
    };
    let HttpFallback::Request(http_payload) = fallback else {
        let _ = send_error(
            client,
            "upstream_http_error",
            "续传请求需要 WebSocket v2，正在重新建立连接",
        )
        .await;
        return true;
    };
    session.state.metrics.record_request_route(traffic.route);
    *active = Some(http::start_http_worker(session, http_payload, traffic));
    true
}
