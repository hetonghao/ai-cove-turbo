use axum::{
    body::Body,
    extract::Request as AxumRequest,
    http::{HeaderMap, Method, Uri, header},
};
use futures_util::StreamExt;
use tokio::sync::mpsc;

use super::{
    Active, WorkerCommand, WorkerEvent,
    common::text_message,
    sse::{SseParser, is_terminal_event},
};
use crate::proxy::{HttpTraffic, ProxyState};

pub(super) fn start_http_worker(
    session: &super::Session,
    payload: Vec<u8>,
    traffic: HttpTraffic,
) -> Active {
    let (command_tx, command_rx) = mpsc::channel(8);
    let (event_tx, event_rx) = mpsc::channel(8);
    let context = WorkerContext {
        state: session.state.clone(),
        request: build_http_request(
            session.client_headers.clone(),
            session.request_uri.clone(),
            payload,
        ),
        traffic,
    };
    let task = tokio::spawn(run_http_worker(context, command_rx, event_tx));
    Active {
        kind: super::ActiveKind::Http,
        commands: command_tx,
        events: event_rx,
        task,
    }
}

struct WorkerContext {
    state: ProxyState,
    request: AxumRequest,
    traffic: HttpTraffic,
}

async fn run_http_worker(
    context: WorkerContext,
    mut commands: mpsc::Receiver<WorkerCommand>,
    events: mpsc::Sender<WorkerEvent>,
) {
    let request_future = super::super::proxy_http(context.state, context.request, context.traffic);
    tokio::pin!(request_future);
    let response = loop {
        tokio::select! {
            biased;
            response = &mut request_future => break response,
            command = commands.recv() => {
                match command {
                    Some(WorkerCommand::Cancel(_)) => {
                        let _ = events.send(WorkerEvent::Cancelled).await;
                        return;
                    }
                    None => return,
                    Some(WorkerCommand::Forward(_, _)) => {}
                }
            }
        }
    };
    if !response.status().is_success() {
        let status = response.status().as_u16();
        let message = format!("HTTP upstream returned status {status}");
        let error = super::common::error_message("upstream_http_error", &message);
        if events.send(WorkerEvent::Message(error)).await.is_err() {
            return;
        }
        let _ = events
            .send(WorkerEvent::Terminal {
                upstream: None,
                response_id: None,
            })
            .await;
        return;
    }

    let mut body = response.into_body().into_data_stream();
    let mut parser = SseParser::default();
    loop {
        tokio::select! {
            biased;
            chunk = body.next() => {
                let Some(chunk) = chunk else {
                    if send_finished_sse_events(&mut parser, &events).await.is_ok_and(|terminal| terminal) {
                        return;
                    }
                    let _ = events.send(WorkerEvent::Error {
                        code: 1011,
                        message: "HTTP stream ended before terminal response event",
                    }).await;
                    return;
                };
                let Ok(chunk) = chunk else {
                    let _ = events.send(WorkerEvent::Error {
                        code: 1011,
                        message: "HTTP response stream failed",
                    }).await;
                    return;
                };
                parser.push(&chunk);
                match send_sse_events(&mut parser, &events).await {
                    Ok(true) | Err(()) => return,
                    Ok(false) => {}
                }
            }
            command = commands.recv() => {
                match command {
                    Some(WorkerCommand::Cancel(_)) => {
                        let _ = events.send(WorkerEvent::Cancelled).await;
                        return;
                    }
                    None => return,
                    Some(WorkerCommand::Forward(_, _)) => {}
                }
            }
        }
    }
}

pub(super) fn build_http_request(headers: HeaderMap, uri: Uri, payload: Vec<u8>) -> AxumRequest {
    let mut request = AxumRequest::new(Body::from(payload));
    *request.method_mut() = Method::POST;
    *request.uri_mut() = uri;
    *request.headers_mut() = headers;
    for name in [
        header::SEC_WEBSOCKET_KEY,
        header::SEC_WEBSOCKET_VERSION,
        header::SEC_WEBSOCKET_PROTOCOL,
        header::SEC_WEBSOCKET_EXTENSIONS,
    ] {
        request.headers_mut().remove(name);
    }
    request.headers_mut().insert(
        header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );
    request
}

pub(super) async fn send_sse_events(
    parser: &mut SseParser,
    events: &mpsc::Sender<WorkerEvent>,
) -> Result<bool, ()> {
    for payload in parser.take_events() {
        let terminal = is_terminal_event(&payload);
        if payload != b"[DONE]" {
            let message = text_message(payload)?;
            events
                .send(WorkerEvent::Message(message))
                .await
                .map_err(|_| ())?;
        }
        if terminal {
            events
                .send(WorkerEvent::Terminal {
                    upstream: None,
                    response_id: None,
                })
                .await
                .map_err(|_| ())?;
            return Ok(true);
        }
    }
    Ok(false)
}

async fn send_finished_sse_events(
    parser: &mut SseParser,
    events: &mpsc::Sender<WorkerEvent>,
) -> Result<bool, ()> {
    let finished = parser.finish();
    let mut parser = SseParser::from_events(finished);
    send_sse_events(&mut parser, events).await
}
