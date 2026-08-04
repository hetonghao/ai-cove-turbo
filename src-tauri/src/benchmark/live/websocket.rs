use std::{
    error::Error,
    io,
    time::{Duration, Instant},
};

use futures_util::{SinkExt, StreamExt};
use reqwest::header::{AUTHORIZATION, HeaderValue};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    time::timeout,
};
use tokio_tungstenite::{
    WebSocketStream, connect_async,
    tungstenite::{client::IntoClientRequest, protocol::Message},
};

use super::super::{
    BenchmarkCase, BenchmarkSettings, RoundSample, Sample, metric_delta, response_is_complete,
};

pub(super) struct Case<'a> {
    pub(super) scenario: &'static str,
    pub(super) path: &'static str,
    pub(super) url: &'a str,
    pub(super) authorization: &'a str,
    pub(super) payloads: &'a [String],
    pub(super) metrics: &'a crate::proxy::Metrics,
}

async fn wait_for_response_complete<S>(
    socket: &mut WebSocketStream<S>,
) -> Result<u64, Box<dyn Error>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut response_events = 0;
    while let Some(message) = socket.next().await {
        match message? {
            Message::Text(text) => {
                response_events += 1;
                if response_is_complete(text.as_ref()) {
                    return Ok(response_events);
                }
            }
            Message::Binary(bytes) => {
                response_events += 1;
                if std::str::from_utf8(&bytes).is_ok_and(response_is_complete) {
                    return Ok(response_events);
                }
            }
            Message::Close(_) => {
                return Err(io::Error::other("WebSocket closed before response completion").into());
            }
            Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {}
        }
    }
    Err(io::Error::other("WebSocket ended before response completion").into())
}

async fn sample(
    url: &str,
    authorization: &str,
    payloads: &[String],
    timeout_duration: Duration,
) -> Result<Sample, Box<dyn Error>> {
    let started = Instant::now();
    let mut request = url.into_client_request()?;
    request.headers_mut().insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {authorization}"))?,
    );
    let (mut socket, response) = timeout(timeout_duration, connect_async(request)).await??;
    if response.status().as_u16() != 101 {
        return Err(io::Error::other(format!(
            "WebSocket benchmark response status {}",
            response.status()
        ))
        .into());
    }
    let setup = started.elapsed();
    let mut round_samples = Vec::with_capacity(payloads.len());
    for payload in payloads {
        let send_started = Instant::now();
        timeout(
            timeout_duration,
            socket.send(Message::Text(payload.to_owned().into())),
        )
        .await??;
        let transport = send_started.elapsed();
        let response_events =
            timeout(timeout_duration, wait_for_response_complete(&mut socket)).await??;
        round_samples.push(RoundSample {
            e2e: send_started.elapsed(),
            transport,
            response_events,
        });
    }
    Ok(Sample {
        e2e: started.elapsed(),
        transport: round_samples.iter().map(|sample| sample.transport).sum(),
        setup,
        raw_bytes: u64::try_from(payloads.iter().map(String::len).sum::<usize>())?,
        wire_bytes: 0,
        logical_requests: u64::try_from(payloads.len())?,
        application_messages: u64::try_from(payloads.len())?,
        response_events: round_samples
            .iter()
            .map(|sample| sample.response_events)
            .sum(),
        websocket_handshakes: 1,
        round_e2e: round_samples.iter().map(|sample| sample.e2e).collect(),
        round_transport: round_samples
            .iter()
            .map(|sample| sample.transport)
            .collect(),
    })
}

pub(super) async fn collect_case(
    case: Case<'_>,
    settings: &BenchmarkSettings,
) -> Result<BenchmarkCase, Box<dyn Error>> {
    let mut samples = Vec::with_capacity(settings.runs);
    for iteration in 0..settings.warmups + settings.runs {
        let before = case.metrics.snapshot();
        let mut sample = sample(
            case.url,
            case.authorization,
            case.payloads,
            settings.timeout,
        )
        .await?;
        let after = case.metrics.snapshot();
        let (raw_bytes, wire_bytes) = metric_delta(before, after, true);
        if raw_bytes == 0 || wire_bytes == 0 {
            return Err(io::Error::other(
                "Turbo did not record the WebSocket application messages",
            )
            .into());
        }
        let handshakes = after
            .websocket_handshakes
            .saturating_sub(before.websocket_handshakes);
        if handshakes != 1 {
            return Err(
                io::Error::other("WebSocket benchmark did not use one handshake per run").into(),
            );
        }
        sample.raw_bytes = raw_bytes;
        sample.wire_bytes = wire_bytes;
        if iteration >= settings.warmups {
            samples.push(sample);
        }
    }
    Ok(BenchmarkCase {
        scenario: case.scenario,
        path: case.path,
        samples,
    })
}
