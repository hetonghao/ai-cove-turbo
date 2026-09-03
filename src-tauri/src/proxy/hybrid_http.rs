use std::{sync::Arc, time::Instant};

use axum::{
    body::Body,
    extract::Request as AxumRequest,
    http::{HeaderMap, Method, Uri, header},
};
use futures_util::StreamExt;
use tokio::sync::mpsc;

use super::super::timing::HttpTimingControl;
use super::{
    Active, WorkerCommand, WorkerEvent,
    common::{context_length_exceeded_message, text_message},
    sse::{SseParser, is_terminal_event, success_terminal_response_id},
};
use crate::proxy::{HttpRequestMetric, HttpTraffic, ProxyState, traffic};

pub(super) fn start_http_worker(
    session: &super::Session,
    payload: Vec<u8>,
    traffic: HttpTraffic,
) -> Active {
    let raw_bytes = u64::try_from(payload.len()).unwrap_or(u64::MAX);
    let (command_tx, command_rx) = mpsc::channel(8);
    let (event_tx, event_rx) = mpsc::channel(8);
    let mut metadata = session
        .request_metadata
        .clone()
        .unwrap_or_else(|| traffic::request_metadata(&session.client_headers, &payload));
    metadata.thread_id = session.thread_id.clone().or(metadata.thread_id);
    session.state.metrics.observe_session_name_hint(&metadata);
    let context = WorkerContext {
        state: session.state.clone(),
        control: Arc::new(HttpTimingControl::default()),
        path: session.path.clone(),
        started_at: Instant::now(),
        raw_bytes,
        metadata,
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
        http_traffic: Some(traffic),
        http_fallback: None,
        output_forwarded: false,
        cancel_requested: false,
        commands: command_tx,
        events: event_rx,
        task,
    }
}

struct WorkerContext {
    state: ProxyState,
    control: Arc<HttpTimingControl>,
    path: String,
    started_at: Instant,
    raw_bytes: u64,
    metadata: traffic::RequestMetadata,
    request: AxumRequest,
    traffic: HttpTraffic,
}

async fn run_http_worker(
    context: WorkerContext,
    mut commands: mpsc::Receiver<WorkerCommand>,
    events: mpsc::Sender<WorkerEvent>,
) {
    let control = Arc::clone(&context.control);
    let Some(response) = wait_for_http_response(context, &mut commands, &events).await else {
        return;
    };
    if !response.status().is_success() {
        control.complete();
        let status = response.status().as_u16();
        let message = format!("HTTP upstream returned status {status}");
        let error = if super::super::is_context_length_exceeded(status) {
            context_length_exceeded_message()
        } else {
            super::common::error_message("upstream_http_error", &message)
        };
        if events.send(WorkerEvent::Message(error)).await.is_err() {
            return;
        }
        let _ = events
            .send(WorkerEvent::Terminal {
                lease: None,
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
                        control.complete();
                        return;
                    }
                    control.fail_stream();
                    let _ = events.send(WorkerEvent::Error {
                        code: 1011,
                        message: "HTTP stream ended before terminal response event".to_owned(),
                    }).await;
                    return;
                };
                let Ok(chunk) = chunk else {
                    control.fail_stream_error();
                    let _ = events.send(WorkerEvent::Error {
                        code: 1011,
                        message: "HTTP response stream failed".to_owned(),
                    }).await;
                    return;
                };
                parser.push(&chunk);
                match send_sse_events(&mut parser, &events).await {
                    Ok(true) => {
                        control.complete();
                        return;
                    }
                    Err(()) => {
                        control.fail_stream();
                        return;
                    }
                    Ok(false) => {}
                }
            }
            command = commands.recv() => {
                match command {
                    Some(WorkerCommand::Cancel(_)) => {
                        control.cancel();
                        let _ = events.send(WorkerEvent::Cancelled { lease: None }).await;
                        return;
                    }
                    None => return,
                    Some(WorkerCommand::Forward(_, _)) => {}
                }
            }
        }
    }
}

async fn wait_for_http_response(
    context: WorkerContext,
    commands: &mut mpsc::Receiver<WorkerCommand>,
    events: &mpsc::Sender<WorkerEvent>,
) -> Option<axum::http::Response<Body>> {
    let state = context.state.clone();
    let control = Arc::clone(&context.control);
    let path = context.path.clone();
    let started_at = context.started_at;
    let raw_bytes = context.raw_bytes;
    let metadata = context.metadata.clone();
    let traffic = context.traffic;
    let request = context.request;
    let request_future = super::super::proxy_http_with_control(
        state.clone(),
        request,
        traffic,
        Some(Arc::clone(&control)),
    );
    tokio::pin!(request_future);
    let response = loop {
        tokio::select! {
            biased;
            response = &mut request_future => break response,
            command = commands.recv() => {
                match command {
                    Some(WorkerCommand::Cancel(_)) => {
                        record_cancelled_before_response(
                            &state,
                            &control,
                            &path,
                            started_at,
                            raw_bytes,
                            traffic,
                            metadata.clone(),
                        );
                        let _ = events.send(WorkerEvent::Cancelled { lease: None }).await;
                        return None;
                    }
                    None => return None,
                    Some(WorkerCommand::Forward(_, _)) => {}
                }
            }
        }
    };
    Some(response)
}

#[allow(clippy::too_many_arguments)]
fn record_cancelled_before_response(
    state: &ProxyState,
    control: &HttpTimingControl,
    path: &str,
    started_at: Instant,
    raw_bytes: u64,
    traffic: HttpTraffic,
    metadata: traffic::RequestMetadata,
) {
    control.cancel();
    if !control.claim_recording() {
        return;
    }
    state.metrics.record_http_with_timing_and_metadata(
        HttpRequestMetric {
            path,
            status: 499,
            raw_bytes: usize::try_from(raw_bytes).unwrap_or(usize::MAX),
            sent_bytes: 0,
            compressed: false,
            result: super::super::traffic::TrafficResult::Error,
            route: traffic.route,
            failure_reason: Some("request cancelled by client"),
        },
        None,
        Some(super::super::timing::elapsed_ms(started_at, Instant::now())),
        Some(metadata),
    );
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
        let response_id = terminal
            .then(|| success_terminal_response_id(&payload))
            .flatten();
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
                    lease: None,
                    response_id,
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
