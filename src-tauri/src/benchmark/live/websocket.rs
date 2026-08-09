use std::{
    io,
    time::{Duration, Instant},
};

use futures_util::{SinkExt, StreamExt};
use reqwest::header::{AUTHORIZATION, HeaderValue};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::time::timeout;
use tokio_tungstenite::{
    WebSocketStream, connect_async,
    tungstenite::{
        Error as WebSocketError,
        client::IntoClientRequest,
        handshake::client::Request,
        protocol::{CloseFrame, Message},
    },
};

use super::super::{
    BenchmarkResult, BenchmarkSettings, Completion, HYBRID_PATH, RoundSample, RoundTransport,
    Sample, WEBSOCKET_PATH, benchmark_error, completion_response_id, metric_delta,
    payload_with_previous_response_id,
};
use crate::proxy::MetricsSnapshot;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Mode {
    PrivateWebSocket,
    Hybrid,
}

impl Mode {
    const fn label(self) -> &'static str {
        match self {
            Self::PrivateWebSocket => WEBSOCKET_PATH,
            Self::Hybrid => HYBRID_PATH,
        }
    }
}

pub(super) struct Case<'a> {
    pub(super) url: &'a str,
    pub(super) authorization: &'a str,
    pub(super) payloads: &'a [String],
    pub(super) metrics: Option<&'a crate::proxy::Metrics>,
    pub(super) mode: Mode,
}

#[derive(Debug, Default)]
struct ResponseTiming {
    first_event: Option<Duration>,
    response_events: u64,
    response_id: Option<String>,
}

fn record_application_message(
    timing: &mut ResponseTiming,
    message: &Message,
    elapsed: Duration,
) -> bool {
    match message {
        Message::Text(_) | Message::Binary(_) => {
            timing.response_events += 1;
            timing.first_event.get_or_insert(elapsed);
            true
        }
        Message::Close(_) | Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => false,
    }
}

pub(super) fn websocket_close_error(frame: Option<&CloseFrame>) -> io::Error {
    frame.map_or_else(
        || io::Error::other("WebSocket closed before response completion without close frame"),
        |frame| {
            let code = u16::from(frame.code);
            let kind = if matches!(code, 1011..=1013) {
                io::ErrorKind::ConnectionAborted
            } else {
                io::ErrorKind::Other
            };
            io::Error::new(
                kind,
                format!(
                    "WebSocket closed before response completion: code={code} ({:?}), reason={}",
                    frame.code, frame.reason,
                ),
            )
        },
    )
}

pub(super) fn websocket_error(error: WebSocketError) -> io::Error {
    let kind = match &error {
        WebSocketError::Http(response)
            if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS
                || response.status().is_server_error() =>
        {
            io::ErrorKind::ConnectionAborted
        }
        _ => io::ErrorKind::Other,
    };
    io::Error::new(kind, error)
}

pub(super) fn websocket_round_error(mode: Mode, round: usize, error: &io::Error) -> io::Error {
    io::Error::new(
        error.kind(),
        format!("{} round {round} failed: {error}", mode.label()),
    )
}

async fn wait_for_response_complete<S>(
    socket: &mut WebSocketStream<S>,
    started: Instant,
) -> BenchmarkResult<ResponseTiming>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut timing = ResponseTiming::default();
    while let Some(message) = socket.next().await {
        let message = message.map_err(benchmark_error)?;
        let completion = match &message {
            Message::Text(text) => completion_response_id(text.as_bytes()),
            Message::Binary(bytes) => completion_response_id(bytes),
            Message::Close(frame) => return Err(websocket_close_error(frame.as_ref())),
            Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => Completion::Pending,
        };
        record_application_message(&mut timing, &message, started.elapsed());
        if let Completion::Complete(response_id) = completion {
            timing.response_id = response_id;
            return Ok(timing);
        }
    }
    Err(io::Error::other(
        "WebSocket stream ended before response completion without close frame",
    ))
}

fn authenticated_request(url: &str, authorization: &str) -> BenchmarkResult<Request> {
    let mut request = url.into_client_request().map_err(benchmark_error)?;
    request.headers_mut().insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {authorization}")).map_err(benchmark_error)?,
    );
    Ok(request)
}

pub(super) fn validate_hybrid_lifecycle(
    http_requests: u64,
    websocket_messages: u64,
    logical_requests: u64,
) -> BenchmarkResult<()> {
    if http_requests >= 1 && http_requests.checked_add(websocket_messages) == Some(logical_requests)
    {
        return Ok(());
    }
    Err(io::Error::other(format!(
        "Hybrid lifecycle requires at least one HTTP request and exact request/message conservation; http_requests={http_requests}, websocket_messages={websocket_messages}, logical_requests={logical_requests}",
    )))
}

pub(super) fn classify_hybrid_round_transport(
    before: MetricsSnapshot,
    after: MetricsSnapshot,
) -> BenchmarkResult<RoundTransport> {
    let http_requests = after.requests.saturating_sub(before.requests);
    let websocket_messages = after
        .websocket_messages
        .saturating_sub(before.websocket_messages);
    match (http_requests, websocket_messages) {
        (1, 0) => Ok(RoundTransport::Http),
        (0, 1) => Ok(RoundTransport::WebSocket),
        _ => Err(io::Error::other(format!(
            "Hybrid round must use exactly one transport; http_requests={http_requests}, websocket_messages={websocket_messages}",
        ))),
    }
}

pub(super) const fn reported_reconnects(
    mode: Mode,
    before: MetricsSnapshot,
    after: MetricsSnapshot,
) -> u64 {
    match mode {
        Mode::PrivateWebSocket => after
            .websocket_handshakes
            .saturating_sub(before.websocket_handshakes)
            .saturating_sub(1),
        Mode::Hybrid => after
            .hybrid_recovery_http
            .saturating_sub(before.hybrid_recovery_http),
    }
}

pub(super) fn validate_hybrid_round_transports(
    transports: &[RoundTransport],
) -> BenchmarkResult<()> {
    if transports.first() == Some(&RoundTransport::Http) {
        return Ok(());
    }
    Err(io::Error::other(
        "Hybrid lifecycle requires the first round to use HTTP",
    ))
}

fn request_bytes(round_samples: &[RoundSample]) -> BenchmarkResult<u64> {
    round_samples.iter().try_fold(0_u64, |total, sample| {
        total
            .checked_add(sample.request_bytes)
            .ok_or_else(|| io::Error::other("WebSocket benchmark byte count overflow"))
    })
}

async fn sample(case: &Case<'_>, settings: &BenchmarkSettings) -> BenchmarkResult<Sample> {
    let started = Instant::now();
    let request = authenticated_request(case.url, case.authorization)?;
    let (mut socket, response) = timeout(settings.timeout, connect_async(request))
        .await
        .map_err(benchmark_error)?
        .map_err(websocket_error)?;
    if response.status().as_u16() != 101 {
        return Err(io::Error::other(format!(
            "WebSocket benchmark response status {}",
            response.status()
        )));
    }
    let setup = started.elapsed();
    let connection_started = Instant::now();
    let mut round_samples = Vec::with_capacity(case.payloads.len());
    let mut round_transports = Vec::with_capacity(case.payloads.len());
    let mut previous_response_id = None;
    for (round, payload) in case.payloads.iter().enumerate() {
        let transport_before = case
            .metrics
            .map(crate::proxy::Metrics::snapshot)
            .unwrap_or_default();
        let round_started = Instant::now();
        let payload = payload_with_previous_response_id(payload, previous_response_id.as_deref())?;
        let request_bytes = u64::try_from(payload.len()).map_err(benchmark_error)?;
        timeout(settings.timeout, socket.send(Message::Text(payload.into())))
            .await
            .map_err(benchmark_error)?
            .map_err(benchmark_error)?;
        let timing = timeout(
            settings.timeout,
            wait_for_response_complete(&mut socket, round_started),
        )
        .await
        .map_err(|error| websocket_round_error(case.mode, round + 1, &benchmark_error(error)))?
        .map_err(|error| websocket_round_error(case.mode, round + 1, &error))?;
        if round + 1 < case.payloads.len() && timing.response_id.is_none() {
            return Err(websocket_round_error(
                case.mode,
                round + 1,
                &io::Error::other("completed response did not include response.id"),
            ));
        }
        previous_response_id.clone_from(&timing.response_id);
        round_samples.push(RoundSample {
            e2e: round_started.elapsed(),
            first_event: timing.first_event,
            response_events: timing.response_events,
            response_id: timing.response_id,
            request_bytes,
        });
        let transport = match case.mode {
            Mode::PrivateWebSocket => RoundTransport::WebSocket,
            Mode::Hybrid => classify_hybrid_round_transport(
                transport_before,
                case.metrics
                    .ok_or_else(|| io::Error::other("Hybrid benchmark requires metrics"))?
                    .snapshot(),
            )?,
        };
        round_transports.push(transport);
    }
    timeout(settings.timeout, socket.close(None))
        .await
        .map_err(benchmark_error)?
        .map_err(benchmark_error)?;
    let logical_requests = u64::try_from(case.payloads.len()).map_err(benchmark_error)?;
    Ok(Sample {
        e2e: started.elapsed(),
        setup,
        raw_bytes: request_bytes(&round_samples)?,
        encoded_bytes: 0,
        logical_requests,
        application_messages: logical_requests,
        http_requests: 0,
        websocket_messages: logical_requests,
        response_events: round_samples
            .iter()
            .map(|sample| sample.response_events)
            .sum(),
        websocket_handshakes: 1,
        round_e2e: round_samples.iter().map(|sample| sample.e2e).collect(),
        first_events: round_samples
            .iter()
            .filter_map(|sample| sample.first_event)
            .collect(),
        warm_round_e2e: round_samples
            .iter()
            .skip(1)
            .map(|sample| sample.e2e)
            .collect(),
        connection_lifetime: Some(connection_started.elapsed()),
        websocket_reconnects: 0,
        messages_per_connection: Some(logical_requests),
        retries: 0,
        round_transports,
    })
}

pub(super) async fn collect_sample(
    case: &Case<'_>,
    settings: &BenchmarkSettings,
) -> BenchmarkResult<Sample> {
    let before = case
        .metrics
        .map(crate::proxy::Metrics::snapshot)
        .unwrap_or_default();
    let mut sample = sample(case, settings).await?;
    let (raw_bytes, encoded_bytes) = if let Some(metrics) = case.metrics {
        let after = metrics.snapshot();
        let (raw_bytes, encoded_bytes) = match case.mode {
            Mode::PrivateWebSocket => metric_delta(before, after, true),
            Mode::Hybrid => {
                let (http_raw_bytes, http_encoded_bytes) = metric_delta(before, after, false);
                let (ws_raw_bytes, ws_encoded_bytes) = metric_delta(before, after, true);
                (
                    http_raw_bytes.saturating_add(ws_raw_bytes),
                    http_encoded_bytes.saturating_add(ws_encoded_bytes),
                )
            }
        };
        if raw_bytes == 0 || encoded_bytes == 0 {
            return Err(io::Error::other(
                "Turbo did not record the WebSocket application messages",
            ));
        }
        let handshakes = after
            .websocket_handshakes
            .saturating_sub(before.websocket_handshakes);
        match (case.mode, handshakes) {
            (Mode::PrivateWebSocket, 1) | (Mode::Hybrid, _) => {}
            (Mode::PrivateWebSocket, _) => {
                return Err(io::Error::other(
                    "WebSocket benchmark did not use one handshake per run",
                ));
            }
        }
        let messages = after
            .websocket_messages
            .saturating_sub(before.websocket_messages);
        match case.mode {
            Mode::PrivateWebSocket if messages != sample.application_messages => {
                return Err(io::Error::other(format!(
                    "Turbo recorded {messages} WebSocket application messages for {} logical requests",
                    sample.application_messages,
                )));
            }
            Mode::PrivateWebSocket => {
                sample.http_requests = 0;
                sample.websocket_messages = messages;
            }
            Mode::Hybrid => {
                let http_requests = after.requests.saturating_sub(before.requests);
                validate_hybrid_lifecycle(http_requests, messages, sample.logical_requests)?;
                validate_hybrid_round_transports(&sample.round_transports)?;
                sample.http_requests = http_requests;
                sample.websocket_messages = messages;
            }
        }
        sample.websocket_handshakes = handshakes;
        sample.websocket_reconnects = reported_reconnects(case.mode, before, after);
        sample.messages_per_connection = Some(sample.websocket_messages);
        (raw_bytes, encoded_bytes)
    } else {
        let raw_bytes = sample.raw_bytes;
        sample.http_requests = 0;
        sample.websocket_messages = sample.logical_requests;
        (raw_bytes, raw_bytes)
    };
    if sample.first_events.len() != case.payloads.len() {
        return Err(io::Error::other(
            "WebSocket response did not emit application data",
        ));
    }
    sample.raw_bytes = raw_bytes;
    sample.encoded_bytes = encoded_bytes;
    Ok(sample)
}

#[cfg(test)]
#[path = "websocket_tests.rs"]
mod tests;
