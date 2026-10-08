use std::{future::Future, time::Duration};

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::{Error as WebSocketError, Message};

use crate::proxy::HttpTraffic;

use super::super::{
    hybrid_pool::{Lease, LeaseRetirement},
    model_policy::Transport,
    traffic,
};
use super::{
    Active, ClientWebSocket, Session,
    common::{close_client, event_type, reject_thread_switch, send_error},
    http, idle, legacy,
    sse::{HttpFallback, http_request_payload},
    websocket,
};

#[allow(clippy::large_futures)]
pub(super) async fn handle_idle(
    client: &mut ClientWebSocket,
    session: &mut Session,
    active_response: &mut Option<Active>,
) -> bool {
    if session.current_model_requires_http() {
        session.release_idle_websocket().await;
    }
    let selection = if session.ready.is_some() {
        let capability_cache = std::sync::Arc::clone(&session.state.capability_cache);
        tokio::select! {
        biased;
        () = capability_cache.changed() => return true,
        selection =
        select_idle(
            client.next(),
            poll_ready(&mut session.ready),
            tokio::time::sleep(super::super::hybrid_pool::KEEPALIVE_INTERVAL),
        )
        => selection,
        }
    } else {
        IdleSelection::Client(client.next().await)
    };
    match selection {
        IdleSelection::Client(message) => {
            handle_idle_client_message(client, session, active_response, message).await
        }
        IdleSelection::Ready(result) => idle::handle_idle_upstream(client, session, result).await,
        IdleSelection::Keepalive => idle::handle_idle_keepalive(session).await,
    }
}

pub(super) enum IdleSelection {
    Client(Option<Result<Message, WebSocketError>>),
    Ready(Option<Result<Message, WebSocketError>>),
    Keepalive,
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
    ready: &mut Option<Lease>,
) -> Option<Result<Message, WebSocketError>> {
    let ready = ready.as_mut()?;
    ready.upstream_mut()?.next().await
}

#[allow(clippy::large_futures)]
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

async fn close_missing_continuation(client: &mut ClientWebSocket) -> bool {
    let message = "Previous response is not available on this websocket";
    let _ = send_error(client, "previous_response_not_found", message).await;
    let _ = close_client(client, 1002, message).await;
    false
}

#[allow(clippy::large_futures, clippy::too_many_lines)]
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
    let payload = super::super::gemini_history::normalize_gemini_function_history(&payload)
        .unwrap_or(payload);
    let payload = super::super::deepseek_history::repair_deepseek_tool_history(
        &payload,
        &session.last_deepseek_tool_calls,
    )
    .unwrap_or(payload);
    session.capture_deepseek_tool_calls = serde_json::from_slice::<serde_json::Value>(&payload)
        .ok()
        .and_then(|value| {
            value
                .get("model")
                .and_then(serde_json::Value::as_str)
                .map(|model| model.starts_with("deepseek"))
        })
        .unwrap_or(false);
    let Ok(prepared) = http_request_payload(&payload) else {
        let _ = send_error(
            client,
            "invalid_request",
            "response.create must be a JSON object",
        )
        .await;
        return true;
    };
    if !prepared.has_request_source {
        return close_missing_continuation(client).await;
    }
    if !session.bind_thread_id(prepared.thread_id).await {
        return reject_thread_switch(client).await;
    }
    let mut metadata = traffic::request_metadata(&session.client_headers, &payload);
    metadata.thread_id = session.thread_id.clone().or(metadata.thread_id);
    metadata.temporary_name = prepared.temporary_name.clone();
    session.state.metrics.observe_session_name_hint(&metadata);
    session.request_metadata = Some(metadata);
    let previous_response_id = prepared.previous_response_id;
    if previous_response_id.is_none() {
        session.policy = session.state.model_policy.reload();
    }
    let fallback = prepared.fallback;
    if previous_response_id.is_some()
        && session.last_response_transport != Some(super::ResponseTransport::Http)
    {
        if session.current_model_requires_http() {
            session.release_idle_websocket().await;
            return close_missing_continuation(client).await;
        }
        checkout_handoff_websocket(session, previous_response_id.as_deref()).await;
    }
    if previous_response_id
        .as_deref()
        .is_some_and(|id| session.last_terminal_response_id.as_deref() != Some(id))
    {
        return close_missing_continuation(client).await;
    }
    if previous_response_id.is_none() {
        session.refresh_capability(&payload);
    }
    if previous_response_id.is_some()
        && session.last_response_transport == Some(super::ResponseTransport::Http)
    {
        let traffic = session
            .last_http_traffic
            .unwrap_or(HttpTraffic::HYBRID_CAPABILITY);
        let Some(http_payload) =
            session.expand_http_continuation(&payload, previous_response_id.as_deref())
        else {
            return close_missing_continuation(client).await;
        };
        session.release_idle_websocket().await;
        start_http_response(session, active, http_payload, traffic);
        return true;
    }
    if previous_response_id.is_none() {
        let explicit_http = policy_requires_http(session, &payload);
        let capability_http = !explicit_http && session.auto_uses_http(&payload);
        if explicit_http || capability_http {
            let traffic = if explicit_http {
                HttpTraffic::HYBRID_POLICY
            } else {
                HttpTraffic::HYBRID_CAPABILITY
            };
            session.release_idle_websocket().await;
            start_http_only_response(session, active, fallback, traffic);
            return true;
        }
    }
    let large_http_request = payload.len() >= session.max_websocket_request_bytes
        && matches!(&fallback, HttpFallback::Request(_));
    if !large_http_request {
        checkout_response_websocket(session).await;
    }
    if session.current_model_requires_http() {
        session.release_idle_websocket().await;
        if previous_response_id.is_some() {
            return close_missing_continuation(client).await;
        }
        start_http_only_response(session, active, fallback, HttpTraffic::HYBRID_CAPABILITY);
        return true;
    }
    if !large_http_request && let Some(lease) = session.ready.take() {
        session.connection_id = session
            .handle
            .leased_connection_id()
            .await
            .map(|connection_id| connection_id.to_string());
        session.handle.record_response_create().await;
        session
            .observe_activity(super::ConnectionActivity::Up)
            .await;
        *active = Some(websocket::start_websocket_worker(
            lease,
            payload,
            original_binary,
            std::sync::Arc::clone(&session.state.metrics),
            session.last_terminal_response_id.clone(),
            fallback,
        ));
        return true;
    }

    let traffic = if large_http_request {
        HttpTraffic::HYBRID_LARGE_REQUEST
    } else if session.handle.has_initialized().await {
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
        return close_missing_continuation(client).await;
    };
    if large_http_request && session.ready.is_some() {
        session
            .retire_idle_upstream(LeaseRetirement::Replacing)
            .await;
    }
    *active = Some(http::start_http_worker(session, http_payload, traffic));
    true
}

fn start_http_only_response(
    session: &mut Session,
    active: &mut Option<Active>,
    fallback: HttpFallback,
    traffic: HttpTraffic,
) {
    let HttpFallback::Request(http_payload) = fallback else {
        return;
    };
    start_http_response(session, active, http_payload, traffic);
}

fn start_http_response(
    session: &mut Session,
    active: &mut Option<Active>,
    payload: Vec<u8>,
    traffic: HttpTraffic,
) {
    *active = Some(http::start_http_worker(session, payload, traffic));
}

fn policy_requires_http(session: &Session, payload: &[u8]) -> bool {
    // 实际路由只由用户配置的模型策略决定。
    session.policy.transport_for_payload(payload) == Transport::Http
}

async fn checkout_handoff_websocket(session: &mut Session, previous_response_id: Option<&str>) {
    if session.ready.is_some() {
        return;
    }
    let (Some(thread_id), Some(response_id)) = (session.thread_id.as_deref(), previous_response_id)
    else {
        return;
    };
    match session
        .handle
        .checkout_handoff_wait(thread_id, response_id)
        .await
    {
        Ok(Some(upstream)) => {
            session.ready = Some(upstream);
            session.last_terminal_response_id = Some(response_id.to_owned());
        }
        Err(_) => {
            session.last_terminal_response_id = None;
        }
        Ok(None) => {}
    }
}

async fn checkout_response_websocket(session: &mut Session) {
    if session.ready.is_none() {
        let stop = async {
            loop {
                if session.current_model_requires_http() {
                    return;
                }
                session.state.capability_cache.changed().await;
            }
        };
        let ready = session
            .handle
            .checkout_wait_until(Duration::from_secs(2), stop)
            .await;
        session.ready = ready;
    }
}
