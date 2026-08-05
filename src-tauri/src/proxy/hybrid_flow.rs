use std::future::Future;

use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::{Error as WebSocketError, Message};

use super::{
    Active, ClientWebSocket, PrivateWebSocket, Session,
    common::{close_client, event_type, send_error},
    http, legacy,
    sse::http_request_payload,
    websocket,
};

pub(super) enum PrewarmSelection {
    Closed,
    Failed,
    Ready(Box<super::PrivateWebSocket>),
}

pub(super) enum IdleSelection {
    Client(Option<Result<Message, WebSocketError>>),
    Prewarm(PrewarmSelection),
    Ready(Option<Result<Message, WebSocketError>>),
}

pub(super) async fn receive_prewarm(
    receiver: &mut mpsc::Receiver<Option<PrivateWebSocket>>,
) -> PrewarmSelection {
    match receiver.recv().await {
        Some(Some(upstream)) => PrewarmSelection::Ready(Box::new(upstream)),
        Some(None) => PrewarmSelection::Failed,
        None => PrewarmSelection::Closed,
    }
}

pub(super) async fn select_idle<Client, Prewarm, Ready>(
    client: Client,
    prewarm: Prewarm,
    ready: Ready,
    prewarm_enabled: bool,
    ready_enabled: bool,
) -> IdleSelection
where
    Client: Future<Output = Option<Result<Message, WebSocketError>>>,
    Prewarm: Future<Output = PrewarmSelection>,
    Ready: Future<Output = Option<Result<Message, WebSocketError>>>,
{
    tokio::select! {
        biased;
        result = prewarm, if prewarm_enabled => IdleSelection::Prewarm(result),
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

    if !session.first_response_pending {
        if let Some(upstream) = session.ready.take().filter(|_| !session.force_http) {
            *active = Some(websocket::start_websocket_worker(
                upstream,
                payload,
                original_binary,
                std::sync::Arc::clone(&session.state.metrics),
                session.path.clone(),
            ));
            return true;
        }
    }

    let Ok(http_payload) = http_request_payload(&payload) else {
        let _ = send_error(
            client,
            "invalid_request",
            "response.create must be a JSON object",
        )
        .await;
        return true;
    };
    session.first_response_pending = false;
    if !session.prewarm_attempted {
        session.start_prewarm();
    }
    session.force_http = false;
    *active = Some(http::start_http_worker(session, http_payload));
    true
}
