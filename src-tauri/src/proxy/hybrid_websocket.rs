use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

use super::{
    Active, ActiveKind, PrivateWebSocket, WorkerCommand, WorkerEvent, common::text_message,
    private_websocket, sse::is_terminal_event,
};
use crate::proxy::Metrics;

pub(super) fn start_websocket_worker(
    upstream: PrivateWebSocket,
    payload: Vec<u8>,
    original_binary: bool,
    metrics: Arc<Metrics>,
    path: String,
) -> Active {
    let (command_tx, command_rx) = mpsc::channel(8);
    let (event_tx, event_rx) = mpsc::channel(8);
    let context = WorkerContext { metrics, path };
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
        commands: command_tx,
        events: event_rx,
        task,
    }
}

struct WorkerContext {
    metrics: Arc<Metrics>,
    path: String,
}

async fn run_websocket_worker(
    context: WorkerContext,
    mut upstream: PrivateWebSocket,
    payload: Vec<u8>,
    original_binary: bool,
    mut commands: mpsc::Receiver<WorkerCommand>,
    events: mpsc::Sender<WorkerEvent>,
) {
    if send_private_application(
        &mut upstream,
        payload,
        original_binary,
        &context.metrics,
        &context.path,
    )
    .await
    .is_err()
    {
        send_worker_error(
            &events,
            &context.metrics,
            1011,
            "private websocket send failed",
        )
        .await;
        return;
    }

    loop {
        tokio::select! {
            biased;
            message = upstream.next() => {
                let Some(message) = message else {
                    send_worker_error(&events, &context.metrics, 1011, "private websocket closed while active").await;
                    return;
                };
                let Ok(message) = message else {
                    send_worker_error(&events, &context.metrics, 1011, "private websocket failed while active").await;
                    return;
                };
                match message {
                    Message::Binary(envelope) => {
                        let Ok(decoded) = private_websocket::decode_private_message_async(envelope).await else {
                            send_worker_error(&events, &context.metrics, 1007, "private websocket response is invalid").await;
                            return;
                        };
                        let terminal = is_terminal_event(&decoded.payload);
                        let Ok(message) = decoded_message(decoded) else {
                            send_worker_error(&events, &context.metrics, 1007, "private websocket response is not UTF-8").await;
                            return;
                        };
                        if events.send(WorkerEvent::Message(message)).await.is_err() {
                            context.metrics.record_websocket_closed();
                            return;
                        }
                        if terminal {
                            let _ = events
                                .send(WorkerEvent::Terminal(Some(Box::new(upstream))))
                                .await;
                            return;
                        }
                    }
                    Message::Ping(payload) => {
                        if upstream.send(Message::Pong(payload)).await.is_err() {
                            send_worker_error(&events, &context.metrics, 1011, "private websocket control frame failed").await;
                            return;
                        }
                    }
                    Message::Pong(_) => {}
                    Message::Close(_) => {
                        send_worker_error(&events, &context.metrics, 1011, "private websocket closed while active").await;
                        return;
                    }
                    Message::Text(_) | Message::Frame(_) => {
                        send_worker_error(&events, &context.metrics, 1002, "private application frame must be binary").await;
                        return;
                    }
                }
            }
            command = commands.recv() => {
                if !handle_worker_command(
                    &mut upstream,
                    command,
                    &context,
                    &events,
                ).await {
                    return;
                }
            }
        }
    }
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
    if send_private_application(
        upstream,
        payload,
        original_binary,
        &context.metrics,
        &context.path,
    )
    .await
    .is_err()
    {
        send_worker_error(events, &context.metrics, 1011, message).await;
        return false;
    }
    true
}

async fn send_worker_error(
    events: &mpsc::Sender<WorkerEvent>,
    metrics: &Metrics,
    code: u16,
    message: &'static str,
) {
    metrics.record_websocket_failure();
    metrics.record_websocket_closed();
    let _ = events.send(WorkerEvent::Error { code, message }).await;
}

async fn send_private_application(
    upstream: &mut PrivateWebSocket,
    payload: Vec<u8>,
    original_binary: bool,
    metrics: &Metrics,
    path: &str,
) -> Result<(), ()> {
    let raw_len = payload.len();
    let encoded = private_websocket::encode_private_message_async(payload, original_binary)
        .await
        .map_err(|_| ())?;
    let sent_len = encoded.bytes.len();
    upstream
        .send(Message::Binary(encoded.bytes.into()))
        .await
        .map_err(|_| ())?;
    metrics.record_websocket_zstd_message(path, raw_len, sent_len, encoded.compressed);
    Ok(())
}

fn decoded_message(decoded: private_websocket::DecodedPrivateMessage) -> Result<Message, ()> {
    if decoded.original_binary {
        return Ok(Message::Binary(decoded.payload.into()));
    }
    text_message(decoded.payload)
}
