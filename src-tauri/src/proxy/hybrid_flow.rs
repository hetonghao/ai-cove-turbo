use std::{future::Future, time::Duration};

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::{Error as WebSocketError, Message};

use crate::proxy::HttpTraffic;

use super::super::hybrid_pool::LeaseRetirement;
use super::{
    Active, ClientWebSocket, Session,
    common::{close_client, event_type, reject_thread_switch, send_error},
    http, idle, legacy,
    sse::{HttpFallback, http_request_payload},
    websocket,
};

pub(super) async fn handle_idle(
    client: &mut ClientWebSocket,
    session: &mut Session,
    active_response: &mut Option<Active>,
) -> bool {
    let selection = if session.ready.is_some() {
        select_idle(
            client.next(),
            poll_ready(&mut session.ready),
            tokio::time::sleep(super::super::hybrid_pool::KEEPALIVE_INTERVAL),
        )
        .await
    } else if session.response_started {
        select_waiting_idle(
            client.next(),
            session
                .state
                .hybrid_pool
                .checkout_ready(&session.pool_scope, session.pool_id),
        )
        .await
    } else {
        IdleSelection::Client(client.next().await)
    };
    match selection {
        IdleSelection::Client(message) => {
            handle_idle_client_message(client, session, active_response, message).await
        }
        IdleSelection::PoolReady(upstream) => {
            session.ready = Some(*upstream);
            session.drain_reconnect_pending = false;
            session.observe_idle().await;
            true
        }
        IdleSelection::Ready(result) => idle::handle_idle_upstream(client, session, result).await,
        IdleSelection::Keepalive => idle::handle_idle_keepalive(session).await,
    }
}

pub(super) enum IdleSelection {
    Client(Option<Result<Message, WebSocketError>>),
    PoolReady(Box<super::PrivateWebSocket>),
    Ready(Option<Result<Message, WebSocketError>>),
    Keepalive,
}

pub(super) async fn select_waiting_idle<Client, PoolReady>(
    client: Client,
    pool_ready: PoolReady,
) -> IdleSelection
where
    Client: Future<Output = Option<Result<Message, WebSocketError>>>,
    PoolReady: Future<Output = super::PrivateWebSocket>,
{
    tokio::select! {
        biased;
        upstream = pool_ready => IdleSelection::PoolReady(Box::new(upstream)),
        message = client => IdleSelection::Client(message),
    }
}

pub(super) async fn select_idle<Client, Ready, Keepalive>(
    client: Client,
    ready: Ready,
    keepalive: Keepalive,
) -> IdleSelection
where
    Client: Future<Output = Option<Result<Message, WebSocketError>>>,
    Ready: Future<Output = Option<Result<Message, WebSocketError>>>,
    Keepalive: Future<Output = ()>,
{
    tokio::select! {
        biased;
        () = keepalive => IdleSelection::Keepalive,
        result = ready => IdleSelection::Ready(result),
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

async fn reject_missing_continuation(
    client: &mut ClientWebSocket,
    session: &mut Session,
    has_request_source: bool,
    previous_response_id: Option<&str>,
) -> bool {
    if has_request_source {
        let Some(previous_response_id) = previous_response_id else {
            return false;
        };
        if session.last_terminal_response_id.as_deref() == Some(previous_response_id) {
            return false;
        }
    }
    session.response_started = false;
    let _ = send_error(
        client,
        "previous_response_not_found",
        "Previous response is not available on this websocket",
    )
    .await;
    true
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

    let Ok(prepared) = http_request_payload(&payload) else {
        let _ = send_error(
            client,
            "invalid_request",
            "response.create must be a JSON object",
        )
        .await;
        return true;
    };
    if reject_missing_continuation(client, session, prepared.has_request_source, None).await {
        return true;
    }
    if !session.bind_thread_id(prepared.thread_id).await {
        return reject_thread_switch(client).await;
    }
    session.response_started = true;
    let previous_response_id = prepared.previous_response_id;
    let fallback = prepared.fallback;
    let wait_for_drain_reconnect = std::mem::take(&mut session.drain_reconnect_pending);
    if session.ready.is_none()
        && let (Some(thread_id), Some(response_id)) = (
            session.thread_id.as_deref(),
            previous_response_id.as_deref(),
        )
        && let Some(upstream) = session
            .state
            .hybrid_pool
            .checkout_handoff_wait(&session.pool_scope, session.pool_id, thread_id, response_id)
            .await
    {
        session.ready = Some(upstream);
        session.last_terminal_response_id = Some(response_id.to_owned());
    }
    if reject_missing_continuation(client, session, true, previous_response_id.as_deref()).await {
        return true;
    }
    if session.ready.is_none() {
        session.ready = session
            .state
            .hybrid_pool
            .checkout(&session.pool_scope, session.pool_id)
            .await;
    }
    if session.ready.is_none()
        && (wait_for_drain_reconnect || matches!(&fallback, HttpFallback::WebSocketRequired))
    {
        session.ready = session
            .state
            .hybrid_pool
            .checkout_wait(&session.pool_scope, session.pool_id, Duration::from_secs(2))
            .await;
    }
    if let Some(upstream) = session.ready.take() {
        session
            .state
            .hybrid_pool
            .record_response_create(&session.pool_scope, session.pool_id)
            .await;
        session
            .observe_activity(super::ConnectionActivity::Up)
            .await;
        *active = Some(websocket::start_websocket_worker(
            upstream,
            payload,
            original_binary,
            std::sync::Arc::clone(&session.state.metrics),
            session.last_terminal_response_id.clone(),
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
        session
            .discard(LeaseRetirement::Recovering {
                reason: "续传请求正在等待可用 WebSocket".to_owned(),
            })
            .await;
        let _ = send_error(
            client,
            "upstream_http_error",
            "续传请求需要 WebSocket v2，正在重新建立连接",
        )
        .await;
        return true;
    };
    *active = Some(http::start_http_worker(session, http_payload, traffic));
    true
}
