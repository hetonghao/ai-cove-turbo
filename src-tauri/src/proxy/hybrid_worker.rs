use futures_util::SinkExt;
use tokio_tungstenite::tungstenite::{Error as WebSocketError, Message};

use super::super::hybrid_pool::{Lease, LeaseRetirement};
use super::common::{close_client, event_type, reject_thread_switch, send_error};
use super::sse::http_request_payload;
use super::{Active, ActiveKind, ClientWebSocket, Session, WorkerCommand, WorkerEvent};
use crate::proxy::traffic::{
    self, FailurePhase, TrafficRecord, TrafficResult, TrafficRoute, TrafficTransport,
};

pub(super) async fn handle_active_client_message(
    client: &mut ClientWebSocket,
    session: &mut Session,
    active: &mut Active,
    message: Option<Result<Message, WebSocketError>>,
) -> bool {
    let Some(Ok(message)) = message else {
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
            forward_active_message(client, session, active, text.as_bytes().to_vec(), false).await
        }
        Message::Binary(payload) => {
            forward_active_message(client, session, active, payload.to_vec(), true).await
        }
    }
}

async fn forward_active_message(
    client: &mut ClientWebSocket,
    session: &mut Session,
    active: &mut Active,
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
        if let Ok(prepared) = http_request_payload(&payload) {
            if !session.bind_thread_id(prepared.thread_id).await {
                return reject_thread_switch(client).await;
            }
        }
        return send_error(
            client,
            "invalid_request",
            "上一条 response.create 尚未结束，不支持并发创建",
        )
        .await;
    }
    let cancel_requested = event_type == "response.cancel";
    let command = if cancel_requested {
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
    if active.kind == ActiveKind::WebSocket {
        session
            .observe_activity(super::ConnectionActivity::Up)
            .await;
    }
    let sent = active.commands.send(command).await.is_ok();
    if sent && cancel_requested {
        active.cancel_requested = true;
    }
    sent
}

pub(super) async fn handle_worker_event(
    client: &mut ClientWebSocket,
    session: &mut Session,
    active: &mut Option<Active>,
    event: Option<WorkerEvent>,
) -> bool {
    let Some(event) = event else {
        return super::transport_fallback::handle_stopped_worker(client).await;
    };
    match event {
        WorkerEvent::Message(message) => {
            let from_websocket = active
                .as_ref()
                .is_some_and(|item| item.kind == ActiveKind::WebSocket);
            if from_websocket {
                mark_websocket_first_frame(session, active.as_ref());
                if session.websocket_first_token_at.is_none() && is_first_output_message(&message) {
                    session.websocket_first_token_at = Some(std::time::Instant::now());
                }
                if let Some(active) = active.as_mut() {
                    active.output_forwarded = true;
                }
                session
                    .observe_activity(super::ConnectionActivity::Down)
                    .await;
            }
            client.send(message).await.is_ok()
        }
        WorkerEvent::WebSocketSent(receipt) => {
            session.websocket_receipt = Some(receipt);
            session.websocket_first_frame_at = None;
            session.websocket_first_token_at = None;
            true
        }
        WorkerEvent::Terminal { lease, response_id } => {
            let Some(finished) = active.take() else {
                return false;
            };
            if finished.kind == ActiveKind::WebSocket {
                record_websocket_outcome(
                    session,
                    super::StatusCode::SWITCHING_PROTOCOLS.as_u16(),
                    None,
                );
                if response_id.is_some() {
                    session.last_terminal_response_id = response_id;
                }
                session.ready = lease.map(|lease| *lease);
                if session.ready.is_some() {
                    session.observe_idle().await;
                } else {
                    session
                        .discard(LeaseRetirement::Recovering {
                            reason: "响应结束后没有可复用连接".to_owned(),
                        })
                        .await;
                }
            }
            true
        }
        WorkerEvent::FailedTerminal {
            response,
            code,
            reason,
        } => {
            if super::super::is_context_length_exceeded(code)
                && let Some(raw_bytes) = session
                    .websocket_receipt
                    .and_then(|receipt| usize::try_from(receipt.raw_bytes).ok())
            {
                session.max_websocket_request_bytes =
                    session.max_websocket_request_bytes.min(raw_bytes);
            }
            mark_websocket_first_frame(session, active.as_ref());
            if !retire_failed_websocket(session, active, code, &reason).await {
                return false;
            }
            client.send(response).await.is_ok()
        }
        WorkerEvent::TransportFallback(fallback) => {
            match super::transport_fallback::apply(session, active, fallback).await {
                super::transport_fallback::Action::Forward(response) => {
                    client.send(response).await.is_ok()
                }
                super::transport_fallback::Action::StartedHttp => true,
                super::transport_fallback::Action::Stop => false,
            }
        }
        WorkerEvent::Cancelled { lease } => {
            handle_cancelled_event(client, session, active, lease).await
        }
        WorkerEvent::Error { code, message } => {
            retire_failed_websocket(session, active, code, &message).await;
            let _ = send_error(client, "server_error", &message).await;
            let _ = close_client(client, code, &message).await;
            false
        }
    }
}

async fn handle_cancelled_event(
    client: &mut ClientWebSocket,
    session: &mut Session,
    active: &mut Option<Active>,
    lease: Option<Box<Lease>>,
) -> bool {
    let was_websocket = active
        .take()
        .is_some_and(|item| item.kind == ActiveKind::WebSocket);
    if was_websocket {
        record_websocket_outcome(session, 499, Some("request cancelled by client"));
    }
    session.ready = lease.map(|lease| *lease);
    if session.ready.is_some() {
        session.observe_idle().await;
    }
    let message = serde_json::json!({
        "type": "response.cancelled",
        "response": {"status": "cancelled"},
    });
    client
        .send(Message::Text(message.to_string().into()))
        .await
        .is_ok()
}

pub(super) async fn retire_failed_websocket(
    session: &mut Session,
    active: &mut Option<Active>,
    code: u16,
    message: &str,
) -> bool {
    let failed = active
        .take()
        .is_some_and(|item| item.kind == ActiveKind::WebSocket);
    if failed {
        record_websocket_outcome(session, code, Some(message));
        session
            .discard(LeaseRetirement::Recovering {
                reason: message.to_owned(),
            })
            .await;
    }
    failed
}

pub(super) fn mark_websocket_first_frame(session: &mut Session, active: Option<&Active>) {
    if active.is_some_and(|item| item.kind == ActiveKind::WebSocket)
        && session.websocket_first_frame_at.is_none()
    {
        session.websocket_first_frame_at = Some(std::time::Instant::now());
    }
}

fn record_websocket_outcome(session: &mut Session, status: u16, failure_reason: Option<&str>) {
    let receipt = session.websocket_receipt.take().unwrap_or_default();
    let first_frame_ms = session
        .websocket_first_frame_at
        .take()
        .zip(receipt.started_at)
        .map(|(first_frame_at, started_at)| {
            super::super::timing::elapsed_ms(started_at, first_frame_at)
        });
    let first_token_ms = session
        .websocket_first_token_at
        .take()
        .zip(receipt.started_at)
        .map(|(first_token_at, started_at)| {
            super::super::timing::elapsed_ms(started_at, first_token_at)
        });
    let duration_ms = receipt
        .started_at
        .map(|started_at| super::super::timing::elapsed_ms(started_at, std::time::Instant::now()));
    let (result, failure_phase) = match failure_reason {
        Some(_) => (TrafficResult::Error, Some(FailurePhase::HybridActive)),
        None => (TrafficResult::Success, None),
    };
    session
        .state
        .metrics
        .record_websocket_outcome_with_first_frame_timing(
            TrafficRecord {
                timestamp_ms: traffic::now_ms(),
                status,
                path: &session.path,
                raw_bytes: receipt.raw_bytes,
                sent_bytes: receipt.sent_bytes,
                transport: TrafficTransport::Ws,
                result,
                route: Some(TrafficRoute::HybridWs),
                failure_phase,
                failure_reason,
            },
            receipt.compressed,
            first_frame_ms,
            first_token_ms,
            duration_ms,
        );
}

fn is_first_output_message(message: &tokio_tungstenite::tungstenite::Message) -> bool {
    let payload = match message {
        tokio_tungstenite::tungstenite::Message::Text(text) => text.as_bytes(),
        tokio_tungstenite::tungstenite::Message::Binary(payload) => payload.as_ref(),
        tokio_tungstenite::tungstenite::Message::Ping(_)
        | tokio_tungstenite::tungstenite::Message::Pong(_)
        | tokio_tungstenite::tungstenite::Message::Close(_)
        | tokio_tungstenite::tungstenite::Message::Frame(_) => return false,
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(payload) else {
        return false;
    };
    value
        .get("type")
        .and_then(serde_json::Value::as_str)
        .is_some_and(super::super::is_first_output_event_type)
}
