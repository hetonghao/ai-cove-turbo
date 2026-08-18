use std::{pin::Pin, sync::Arc, time::Duration};

use futures_util::{SinkExt, StreamExt};
use tokio::{
    sync::mpsc,
    time::{Instant, Sleep},
};
use tokio_tungstenite::tungstenite::{Bytes, Message, protocol::CloseFrame};

use super::{
    Active, ActiveKind, PrivateWebSocket, TransportFallback, WebSocketSendReceipt, WorkerCommand,
    WorkerEvent,
    common::{context_length_exceeded_message, event_type, text_message},
    private_websocket,
    sse::{HttpFallback, is_terminal_event, success_terminal_response_id},
};
use crate::proxy::Metrics;

const ACTIVE_KEEPALIVE_IDLE: Duration = Duration::from_secs(30);
const ACTIVE_KEEPALIVE_PAYLOAD: &[u8] = b"turbo-hybrid-active";

pub(super) fn start_websocket_worker(
    upstream: PrivateWebSocket,
    payload: Vec<u8>,
    original_binary: bool,
    metrics: Arc<Metrics>,
    previous_response_id: Option<String>,
    fallback: HttpFallback,
) -> Active {
    let (command_tx, command_rx) = mpsc::channel(8);
    let (event_tx, event_rx) = mpsc::channel(8);
    let context = WorkerContext {
        metrics,
        previous_response_id,
    };
    let task = tokio::spawn(run_websocket_worker(
        context,
        upstream,
        payload,
        original_binary,
        command_rx,
        event_tx,
    ));
    Active {
        kind: ActiveKind::WebSocket,
        http_fallback: match fallback {
            HttpFallback::Request(payload) => Some(payload),
            HttpFallback::WebSocketRequired => None,
        },
        output_forwarded: false,
        cancel_requested: false,
        commands: command_tx,
        events: event_rx,
        task,
    }
}

struct WorkerContext {
    metrics: Arc<Metrics>,
    previous_response_id: Option<String>,
}

enum BinaryOutcome {
    Continue,
    Failure(WorkerEvent),
    Stop,
    Terminal(Option<String>),
}

async fn run_websocket_worker(
    context: WorkerContext,
    mut upstream: PrivateWebSocket,
    payload: Vec<u8>,
    original_binary: bool,
    mut commands: mpsc::Receiver<WorkerCommand>,
    events: mpsc::Sender<WorkerEvent>,
) {
    let Ok(receipt) = send_private_application(&mut upstream, payload, original_binary).await
    else {
        send_worker_error(&events, &context, 1011, "private websocket send failed").await;
        return;
    };
    if events
        .send(WorkerEvent::WebSocketSent(receipt))
        .await
        .is_err()
    {
        context.metrics.record_websocket_closed();
        return;
    }
    let keepalive = tokio::time::sleep(ACTIVE_KEEPALIVE_IDLE);
    tokio::pin!(keepalive);
    let mut awaiting_pong = false;

    loop {
        tokio::select! {
            biased;
            message = upstream.next() => {
                let Some(message) = message else {
                    send_worker_error(&events, &context, 1011, "private websocket closed while active").await;
                    return;
                };
                let Ok(message) = message else {
                    send_worker_error(&events, &context, 1011, "private websocket failed while active").await;
                    return;
                };
                awaiting_pong = false;
                reset_keepalive(keepalive.as_mut(), ACTIVE_KEEPALIVE_IDLE);
                match message {
                    Message::Binary(envelope) => match handle_binary_response(envelope, &events, &context).await {
                        BinaryOutcome::Continue => {}
                        BinaryOutcome::Failure(event) => {
                            context.metrics.record_websocket_closed();
                            drop(upstream);
                            let _ = events.send(event).await;
                            return;
                        }
                        BinaryOutcome::Stop => return,
                        BinaryOutcome::Terminal(response_id) => {
                            let _ = events
                                .send(WorkerEvent::Terminal {
                                    upstream: Some(Box::new(upstream)),
                                    response_id,
                                })
                                .await;
                            return;
                        }
                    },
                    Message::Ping(payload) => {
                        if upstream.send(Message::Pong(payload)).await.is_err() {
                            send_worker_error(&events, &context, 1011, "private websocket control frame failed").await;
                            return;
                        }
                    }
                    Message::Pong(_) => {}
                    Message::Close(frame) => {
                        handle_active_close(frame, &events, &context).await;
                        return;
                    }
                    Message::Text(_) | Message::Frame(_) => {
                        send_worker_error(&events, &context, 1002, "private application frame must be binary").await;
                        return;
                    }
                }
            }
            command = commands.recv() => {
                if !handle_worker_command(&mut upstream, command, &context, &events).await {
                    return;
                }
                awaiting_pong = false;
                reset_keepalive(keepalive.as_mut(), ACTIVE_KEEPALIVE_IDLE);
            }
            () = &mut keepalive => {
                if awaiting_pong {
                    send_worker_error(&events, &context, 1011, "private websocket keepalive timed out").await;
                    return;
                }
                let payload = Bytes::from_static(ACTIVE_KEEPALIVE_PAYLOAD);
                if upstream.send(Message::Ping(payload)).await.is_err() {
                    send_worker_error(&events, &context, 1011, "private websocket keepalive failed").await;
                    return;
                }
                awaiting_pong = true;
                reset_keepalive(
                    keepalive.as_mut(),
                    super::super::hybrid_pool::PONG_TIMEOUT,
                );
            }
        }
    }
}

async fn handle_active_close(
    frame: Option<CloseFrame>,
    events: &mpsc::Sender<WorkerEvent>,
    context: &WorkerContext,
) {
    let Some(frame) = frame else {
        send_worker_error(
            events,
            context,
            1011,
            "private websocket closed while active",
        )
        .await;
        return;
    };
    let code = u16::from(frame.code);
    let reason = if frame.reason.is_empty() {
        format!("private websocket closed while active ({code})")
    } else {
        frame.reason.to_string()
    };
    if super::super::is_context_length_exceeded(code) {
        context.metrics.record_websocket_closed();
        let _ = events
            .send(WorkerEvent::FailedTerminal {
                response: context_length_exceeded_message(),
                code,
                reason,
            })
            .await;
    } else {
        send_worker_error(events, context, code, &reason).await;
    }
}

async fn handle_binary_response(
    envelope: Bytes,
    events: &mpsc::Sender<WorkerEvent>,
    context: &WorkerContext,
) -> BinaryOutcome {
    let Ok(decoded) = private_websocket::decode_private_message_async(envelope).await else {
        send_worker_error(
            events,
            context,
            1007,
            "private websocket response is invalid",
        )
        .await;
        return BinaryOutcome::Stop;
    };
    let terminal = is_terminal_event(&decoded.payload);
    let failed_terminal =
        event_type(&decoded.payload).is_ok_and(|event_type| event_type == "error");
    let transport_fallback = failed_terminal && is_http_transport_fallback(&decoded.payload);
    let failed_diagnostic = failed_terminal.then(|| failed_terminal_diagnostic(&decoded.payload));
    let response_id = terminal
        .then(|| success_terminal_response_id(&decoded.payload))
        .flatten();
    if response_id.is_some() && response_id.as_ref() == context.previous_response_id.as_ref() {
        return BinaryOutcome::Continue;
    }
    let Ok(message) = decoded_message(decoded) else {
        send_worker_error(
            events,
            context,
            1007,
            "private websocket response is not UTF-8",
        )
        .await;
        return BinaryOutcome::Stop;
    };
    if let Some((code, reason)) = failed_diagnostic {
        if transport_fallback {
            return BinaryOutcome::Failure(WorkerEvent::TransportFallback(TransportFallback {
                response: message,
                code,
                reason,
            }));
        }
        let response = if super::super::is_context_length_exceeded(code) {
            context_length_exceeded_message()
        } else {
            message
        };
        return BinaryOutcome::Failure(WorkerEvent::FailedTerminal {
            response,
            code,
            reason,
        });
    }
    if events.send(WorkerEvent::Message(message)).await.is_err() {
        context.metrics.record_websocket_closed();
        return BinaryOutcome::Stop;
    }
    if terminal {
        return BinaryOutcome::Terminal(response_id);
    }
    BinaryOutcome::Continue
}

fn is_http_transport_fallback(payload: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(payload).is_ok_and(|value| {
        value.get("status").and_then(serde_json::Value::as_u64) == Some(503)
            && value.get("transport").and_then(serde_json::Value::as_str) == Some("http")
            && value
                .get("request_state")
                .and_then(serde_json::Value::as_str)
                == Some("not_submitted")
            && value
                .pointer("/error/code")
                .and_then(serde_json::Value::as_str)
                == Some("responses_websocket_unavailable")
    })
}

fn reset_keepalive(keepalive: Pin<&mut Sleep>, after: Duration) {
    keepalive.reset(Instant::now() + after);
}

fn failed_terminal_diagnostic(payload: &[u8]) -> (u16, String) {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(payload) else {
        return (1011, "upstream returned a failed terminal event".to_owned());
    };
    let status = value
        .get("status")
        .and_then(serde_json::Value::as_u64)
        .and_then(|status| u16::try_from(status).ok())
        .filter(|status| *status >= 400)
        .unwrap_or(1011);
    let code = value
        .pointer("/error/code")
        .and_then(serde_json::Value::as_str);
    let message = value
        .pointer("/error/message")
        .and_then(serde_json::Value::as_str);
    let reason = match (code, message) {
        (Some(code), Some(message)) => format!("{code}: {message}"),
        (Some(code), None) => code.to_owned(),
        (None, Some(message)) => message.to_owned(),
        (None, None) => "upstream returned a failed terminal event".to_owned(),
    };
    (status, reason)
}

async fn handle_worker_command(
    upstream: &mut PrivateWebSocket,
    command: Option<WorkerCommand>,
    context: &WorkerContext,
    events: &mpsc::Sender<WorkerEvent>,
) -> bool {
    let Some(command) = command else {
        let _ = upstream.close(None).await;
        context.metrics.record_websocket_closed();
        return false;
    };
    let (payload, original_binary, message) = match command {
        WorkerCommand::Cancel(payload) => (payload, false, "private websocket cancellation failed"),
        WorkerCommand::Forward(payload, original_binary) => {
            (payload, original_binary, "private websocket send failed")
        }
    };
    if send_private_application(upstream, payload, original_binary)
        .await
        .is_err()
    {
        send_worker_error(events, context, 1011, message).await;
        return false;
    }
    true
}

async fn send_worker_error(
    events: &mpsc::Sender<WorkerEvent>,
    context: &WorkerContext,
    code: u16,
    message: &str,
) {
    context.metrics.record_websocket_closed();
    let _ = events
        .send(WorkerEvent::Error {
            code,
            message: message.to_owned(),
        })
        .await;
}

async fn send_private_application(
    upstream: &mut PrivateWebSocket,
    payload: Vec<u8>,
    original_binary: bool,
) -> Result<WebSocketSendReceipt, ()> {
    let raw_bytes = u64::try_from(payload.len()).map_err(|_| ())?;
    let encoded = private_websocket::encode_private_message_async(payload, original_binary)
        .await
        .map_err(|_| ())?;
    let sent_bytes = u64::try_from(encoded.bytes.len()).map_err(|_| ())?;
    let compressed = encoded.compressed;
    upstream
        .send(Message::Binary(encoded.bytes.into()))
        .await
        .map_err(|_| ())?;
    Ok(WebSocketSendReceipt {
        raw_bytes,
        sent_bytes,
        compressed,
    })
}

fn decoded_message(decoded: private_websocket::DecodedPrivateMessage) -> Result<Message, ()> {
    if decoded.original_binary {
        return Ok(Message::Binary(decoded.payload.into()));
    }
    text_message(decoded.payload)
}
