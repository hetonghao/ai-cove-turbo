use futures_util::SinkExt;
use tokio_tungstenite::tungstenite::{Error as WebSocketError, Message};

use super::common::{close_client, event_type, send_error};
use super::{Active, ActiveKind, ClientWebSocket, Session, WorkerCommand, WorkerEvent};

pub(super) async fn handle_active_client_message(
    client: &mut ClientWebSocket,
    active: &Active,
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
            forward_active_message(client, active, text.as_bytes().to_vec(), false).await
        }
        Message::Binary(payload) => {
            forward_active_message(client, active, payload.to_vec(), true).await
        }
    }
}

async fn forward_active_message(
    client: &mut ClientWebSocket,
    active: &Active,
    payload: Vec<u8>,
    original_binary: bool,
) -> bool {
    let Ok(event_type) = event_type(&payload) else {
        let _ = send_error(
            client,
            "invalid_request",
            "WebSocket event must be valid JSON",
        )
        .await;
        return true;
    };
    if event_type == "response.create" {
        return send_error(
            client,
            "invalid_request",
            "上一条 response.create 尚未结束，不支持并发创建",
        )
        .await;
    }
    let command = if event_type == "response.cancel" {
        WorkerCommand::Cancel(payload)
    } else if active.kind == ActiveKind::WebSocket {
        WorkerCommand::Forward(payload, original_binary)
    } else {
        let _ = send_error(
            client,
            "invalid_request",
            "HTTP response only accepts response.cancel while active",
        )
        .await;
        return true;
    };
    active.commands.send(command).await.is_ok()
}

pub(super) async fn handle_worker_event(
    client: &mut ClientWebSocket,
    session: &mut Session,
    active: &mut Option<Active>,
    event: Option<WorkerEvent>,
) -> bool {
    let Some(event) = event else {
        let _ = send_error(
            client,
            "server_error",
            "response worker stopped unexpectedly",
        )
        .await;
        let _ = close_client(client, 1011, "response worker stopped unexpectedly").await;
        return false;
    };
    match event {
        WorkerEvent::Message(message) => client.send(message).await.is_ok(),
        WorkerEvent::Terminal(upstream) => {
            let Some(finished) = active.take() else {
                return false;
            };
            if finished.kind == ActiveKind::WebSocket {
                session.ready = upstream.map(|upstream| *upstream);
            }
            true
        }
        WorkerEvent::Cancelled => {
            active.take();
            session.abort_prewarm();
            session.prewarm_attempted = false;
            let message = serde_json::json!({
                "type": "response.cancelled",
                "response": {"status": "cancelled"},
            });
            client
                .send(Message::Text(message.to_string().into()))
                .await
                .is_ok()
        }
        WorkerEvent::Error { code, message } => {
            let finished = active.take();
            if finished
                .as_ref()
                .is_some_and(|item| item.kind == ActiveKind::WebSocket)
            {
                session.ready = None;
            }
            let _ = send_error(client, "server_error", message).await;
            let _ = close_client(client, code, message).await;
            false
        }
    }
}

pub(super) async fn handle_idle_upstream(
    client: &mut ClientWebSocket,
    session: &mut Session,
    message: Option<Result<Message, WebSocketError>>,
) -> bool {
    let Some(message) = message else {
        session.ready.take();
        session.state.metrics.record_websocket_failure();
        session.state.metrics.record_websocket_closed();
        let _ = close_client(client, 1011, "idle upstream websocket closed").await;
        return false;
    };
    let Ok(message) = message else {
        session.ready.take();
        session.state.metrics.record_websocket_failure();
        session.state.metrics.record_websocket_closed();
        let _ = close_client(client, 1011, "idle upstream websocket failed").await;
        return false;
    };
    match message {
        Message::Ping(payload) => {
            let Some(ready) = session.ready.as_mut() else {
                return false;
            };
            ready.send(Message::Pong(payload)).await.is_ok()
        }
        Message::Pong(_) => true,
        Message::Close(frame) => {
            let code = frame.as_ref().map_or(1000, |frame| u16::from(frame.code));
            session.ready.take();
            session.state.metrics.record_websocket_closed();
            if code == 1012 {
                session.state.metrics.record_websocket_failure();
                session.prewarm_attempted = false;
                session.force_http = true;
                true
            } else {
                let _ = close_client(client, code, "idle upstream websocket closed").await;
                false
            }
        }
        Message::Text(_) | Message::Binary(_) | Message::Frame(_) => {
            session.ready.take();
            session
                .state
                .metrics
                .record_websocket_error(&session.path, 1002);
            session.state.metrics.record_websocket_closed();
            let _ = close_client(client, 1002, "unexpected idle upstream message").await;
            false
        }
    }
}

pub(super) fn cleanup_session(session: &mut Session, active: &mut Option<Active>) {
    if let Some(active) = active.take() {
        active.task.abort();
        if active.kind == ActiveKind::WebSocket {
            session.state.metrics.record_websocket_closed();
        }
    }
    session.abort_prewarm();
    session.close_ready();
}
