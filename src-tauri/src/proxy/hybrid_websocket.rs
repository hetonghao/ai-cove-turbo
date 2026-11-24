use std::{
    pin::Pin,
    time::{Duration, Instant as StdInstant},
};

use futures_util::{SinkExt, StreamExt};
use tokio::{
    sync::mpsc,
    time::{Instant, Sleep},
};
use tokio_tungstenite::tungstenite::Error as WebSocketError;
use tokio_tungstenite::tungstenite::{Bytes, Message, protocol::CloseFrame};

use super::super::hybrid_pool::Lease;
use super::{
    Active, ActiveKind, PrivateWebSocket, TransportFallback, WebSocketSendReceipt, WorkerCommand,
    WorkerEvent,
    common::{context_length_exceeded_message, event_type, text_message},
    private_websocket,
    sse::{HttpFallback, is_terminal_event, success_terminal_response_id},
};

const ACTIVE_KEEPALIVE_IDLE: Duration = Duration::from_secs(30);
const ACTIVE_KEEPALIVE_PAYLOAD: &[u8] = b"turbo-hybrid-active";

pub(super) fn start_websocket_worker(
    lease: Lease,
    payload: Vec<u8>,
    original_binary: bool,
    session: &super::Session,
    fallback: HttpFallback,
    entered_at: StdInstant,
) -> Active {
    let (command_tx, command_rx) = mpsc::channel(8);
    let (event_tx, event_rx) = mpsc::channel(8);
    let context = WorkerContext {
        original: (entered_at, payload.len()),
        state: session.state.clone(),
        headers: session.client_headers.clone(),
        model: session
            .request_metadata
            .as_ref()
            .and_then(|m| m.model.clone()),
        previous_response_id: session.last_terminal_response_id.clone(),
    };
    let task = tokio::spawn(run_websocket_worker(
        context,
        lease,
        payload,
        original_binary,
        command_rx,
        event_tx,
    ));
    Active {
        kind: ActiveKind::WebSocket,
        http_traffic: None,
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
    original: (StdInstant, usize),
    state: crate::proxy::ProxyState,
    headers: axum::http::HeaderMap,
    model: Option<String>,
    previous_response_id: Option<String>,
}

enum BinaryOutcome {
    Continue,
    Failure(Box<WorkerEvent>),
    Stop,
    Terminal(Option<String>),
    Cancelled,
}

enum SendFailure {
    PayloadSize,
    Encoding,
    WebSocket(&'static str),
}

impl SendFailure {
    const fn reason(&self) -> &'static str {
        match self {
            Self::PayloadSize => "payload_size",
            Self::Encoding => "encoding",
            Self::WebSocket(kind) => kind,
        }
    }
}

async fn run_websocket_worker(
    context: WorkerContext,
    mut lease: Lease,
    payload: Vec<u8>,
    original_binary: bool,
    mut commands: mpsc::Receiver<WorkerCommand>,
    events: mpsc::Sender<WorkerEvent>,
) {
    let mut waiting = Vec::new();
    let mut waiting_bytes = 0usize;
    let image_work = context
        .state
        .compression_enabled
        .load(std::sync::atomic::Ordering::Relaxed)
        && payload.windows(5).any(|part| part == b"data:");
    let (frame, report) = {
        let readied = async {
            let (body, report) = context
                .state
                .prepare_images(payload.into(), &context.headers, context.model.as_deref())
                .await;
            let marker = report
                .as_ref()
                .and_then(crate::image_processing::Report::marker);
            let payload = crate::image_processing::ws_metadata(body.into(), marker.as_deref());
            let frame = private_websocket::encode_outbound(
                payload,
                original_binary,
                context
                    .state
                    .compression_enabled
                    .load(std::sync::atomic::Ordering::Relaxed),
            )
            .await;
            (frame, report)
        };
        tokio::pin!(readied);
        if !image_work {
            readied.await
        } else {
            loop {
                tokio::select! {biased;
                    cmd=commands.recv()=> match cmd {
                        Some(WorkerCommand::Cancel(_))=>{let _=events.send(WorkerEvent::Cancelled{lease:Some(Box::new(lease))}).await;return},
                        None=>return,
                        Some(WorkerCommand::Forward(data,bin))=>{waiting_bytes=waiting_bytes.saturating_add(data.len()); if waiting.len()>=8 || waiting_bytes>1024*1024 {send_worker_error(&events,&context,1009,"too many events before submission").await;return} waiting.push(WorkerCommand::Forward(data,bin));}
                    }, result=&mut readied=>break result,
                }
            }
        }
    };
    let Ok(frame) = frame else {
        send_worker_error(&events, &context, 1011, "private websocket encoding failed").await;
        return;
    };
    let receipt = WebSocketSendReceipt {
        raw_bytes: context.original.1 as u64,
        sent_bytes: frame.bytes.len() as u64,
        compressed: frame.compressed,
        started_at: Some(context.original.0),
    };
    let Some(upstream) = lease.upstream_mut() else {
        send_worker_error(&events, &context, 1011, "private websocket lease is empty").await;
        return;
    };
    if let Err(err) = upstream.send(Message::Binary(frame.bytes.into())).await {
        let reason = format!(
            "private websocket send failed · {}",
            private_websocket::websocket_error_kind(&err)
        );
        send_worker_error(&events, &context, 1011, &reason).await;
        return;
    }
    if let Some(report) = report
        && events
            .send(WorkerEvent::ImagesPrepared(report))
            .await
            .is_err()
    {
        return;
    }
    for command in waiting {
        if !handle_worker_command(&mut lease, Some(command), &context, &events).await {
            return;
        }
    }
    if events
        .send(WorkerEvent::WebSocketSent(receipt))
        .await
        .is_err()
    {
        context.state.metrics.record_websocket_closed();
        return;
    }
    run_websocket_loop(context, lease, commands, events).await;
}

async fn run_websocket_loop(
    context: WorkerContext,
    mut lease: Lease,
    mut commands: mpsc::Receiver<WorkerCommand>,
    events: mpsc::Sender<WorkerEvent>,
) {
    let keepalive = tokio::time::sleep(ACTIVE_KEEPALIVE_IDLE);
    tokio::pin!(keepalive);
    let mut awaiting_pong = false;
    loop {
        tokio::select! {
            biased;
            message = async {
                let upstream = lease.upstream_mut()?;
                upstream.next().await
            } => {
                let Some(message) = resolve_active_message(message, &events, &context).await else { return; };
                awaiting_pong = false;
                reset_keepalive(keepalive.as_mut(), ACTIVE_KEEPALIVE_IDLE);
                match message {
                    Message::Binary(envelope) => match handle_binary_response(envelope, &events, &context).await {
                        BinaryOutcome::Continue => {}
                        BinaryOutcome::Failure(event) => {
                            context.state.metrics.record_websocket_closed();
                            let _ = events.send(*event).await;
                            return;
                        }
                        BinaryOutcome::Stop => return,
                        BinaryOutcome::Terminal(response_id) => {
                            let _ = events.send(WorkerEvent::Terminal { lease: Some(Box::new(lease)), response_id }).await;
                            return;
                        }
                        BinaryOutcome::Cancelled => {
                            let _ = events
                                .send(WorkerEvent::Cancelled {
                                    lease: Some(Box::new(lease)),
                                })
                                .await;
                            return;
                        }
                    },
                    Message::Ping(payload) => {
                        let Some(upstream) = lease.upstream_mut() else {
                            send_worker_error(&events, &context, 1011, "private websocket lease is empty").await;
                            return;
                        };
                        if let Err(error) = upstream.send(Message::Pong(payload)).await {
                            let reason = format!("private websocket control frame failed · {}", private_websocket::websocket_error_kind(&error));
                            send_worker_error(&events, &context, 1011, &reason).await;
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
                if !handle_worker_command(&mut lease, command, &context, &events).await { return; }
                awaiting_pong = false;
                reset_keepalive(keepalive.as_mut(), ACTIVE_KEEPALIVE_IDLE);
            }
            () = &mut keepalive => {
                if !handle_keepalive(
                    &mut lease,
                    &events,
                    &context,
                    &mut awaiting_pong,
                    keepalive.as_mut(),
                )
                .await
                {
                    return;
                }
            }
        }
    }
}

async fn handle_keepalive(
    lease: &mut Lease,
    events: &mpsc::Sender<WorkerEvent>,
    context: &WorkerContext,
    awaiting_pong: &mut bool,
    keepalive: Pin<&mut Sleep>,
) -> bool {
    if *awaiting_pong {
        send_worker_error(
            events,
            context,
            1011,
            "private websocket keepalive timed out",
        )
        .await;
        return false;
    }
    let payload = Bytes::from_static(ACTIVE_KEEPALIVE_PAYLOAD);
    let Some(upstream) = lease.upstream_mut() else {
        send_worker_error(events, context, 1011, "private websocket lease is empty").await;
        return false;
    };
    if let Err(error) = upstream.send(Message::Ping(payload)).await {
        let reason = format!(
            "private websocket keepalive failed · {}",
            private_websocket::websocket_error_kind(&error)
        );
        send_worker_error(events, context, 1011, &reason).await;
        return false;
    }
    *awaiting_pong = true;
    reset_keepalive(keepalive, super::super::hybrid_pool::PONG_TIMEOUT);
    true
}

async fn resolve_active_message(
    message: Option<Result<Message, WebSocketError>>,
    events: &mpsc::Sender<WorkerEvent>,
    context: &WorkerContext,
) -> Option<Message> {
    match message {
        None => {
            send_worker_error(
                events,
                context,
                1011,
                "private websocket closed while active · connection_closed",
            )
            .await;
            None
        }
        Some(Ok(message)) => Some(message),
        Some(Err(error)) => {
            let reason = format!(
                "private websocket failed while active · {}",
                private_websocket::websocket_error_kind(&error)
            );
            send_worker_error(events, context, 1011, &reason).await;
            None
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
            "private websocket closed while active · connection_closed",
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
        context.state.metrics.record_websocket_closed();
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
    if decoded.payload.len() < 256
        && event_type(&decoded.payload).is_ok_and(|v| v == "ai_cove.image_receipt")
    {
        if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&decoded.payload)
            && let Some(id) = value
                .get("request_id")
                .and_then(serde_json::Value::as_str)
                .filter(|id| !id.is_empty() && id.len() <= 128)
        {
            let _ = events
                .send(WorkerEvent::PlatformRequestId(id.to_owned()))
                .await;
        }
        return BinaryOutcome::Continue;
    }
    let terminal = is_terminal_event(&decoded.payload);
    let cancelled_terminal = event_type(&decoded.payload).is_ok_and(|event_type| {
        matches!(
            event_type.as_str(),
            "response.cancelled" | "response.canceled"
        )
    });
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
            return BinaryOutcome::Failure(Box::new(WorkerEvent::TransportFallback(
                TransportFallback {
                    response: message,
                    code,
                    reason,
                },
            )));
        }
        let response = if super::super::is_context_length_exceeded(code) {
            context_length_exceeded_message()
        } else {
            message
        };
        return BinaryOutcome::Failure(Box::new(WorkerEvent::FailedTerminal {
            response,
            code,
            reason,
        }));
    }
    if cancelled_terminal {
        return BinaryOutcome::Cancelled;
    }
    if events.send(WorkerEvent::Message(message)).await.is_err() {
        context.state.metrics.record_websocket_closed();
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
    lease: &mut Lease,
    command: Option<WorkerCommand>,
    context: &WorkerContext,
    events: &mpsc::Sender<WorkerEvent>,
) -> bool {
    let Some(command) = command else {
        if let Some(upstream) = lease.upstream_mut() {
            let _ = upstream.close(None).await;
        }
        context.state.metrics.record_websocket_closed();
        return false;
    };
    let (payload, original_binary, message) = match command {
        WorkerCommand::Cancel(payload) => (payload, false, "private websocket cancellation failed"),
        WorkerCommand::Forward(payload, original_binary) => {
            (payload, original_binary, "private websocket send failed")
        }
    };
    let Some(upstream) = lease.upstream_mut() else {
        send_worker_error(events, context, 1011, "private websocket lease is empty").await;
        return false;
    };
    if let Err(failure) = send_private_application(
        upstream,
        payload,
        original_binary,
        context
            .state
            .compression_enabled
            .load(std::sync::atomic::Ordering::Relaxed),
    )
    .await
    {
        let reason = format!("{message} · {}", failure.reason());
        send_worker_error(events, context, 1011, &reason).await;
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
    context.state.metrics.record_websocket_closed();
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
    compression: bool,
) -> Result<WebSocketSendReceipt, SendFailure> {
    let started_at = StdInstant::now();
    let raw_bytes = u64::try_from(payload.len()).map_err(|_| SendFailure::PayloadSize)?;
    let encoded = private_websocket::encode_outbound(payload, original_binary, compression)
        .await
        .map_err(|_| SendFailure::Encoding)?;
    let sent_bytes = u64::try_from(encoded.bytes.len()).map_err(|_| SendFailure::PayloadSize)?;
    let compressed = encoded.compressed;
    upstream
        .send(Message::Binary(encoded.bytes.into()))
        .await
        .map_err(|error| SendFailure::WebSocket(private_websocket::websocket_error_kind(&error)))?;
    Ok(WebSocketSendReceipt {
        raw_bytes,
        sent_bytes,
        compressed,
        started_at: Some(started_at),
    })
}

fn decoded_message(decoded: private_websocket::DecodedPrivateMessage) -> Result<Message, ()> {
    if decoded.original_binary {
        return Ok(Message::Binary(decoded.payload.into()));
    }
    text_message(decoded.payload)
}
