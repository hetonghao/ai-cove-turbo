use std::{
    error::Error,
    io,
    time::{Duration, Instant},
};

use futures_util::{SinkExt, StreamExt};
use reqwest::header::{AUTHORIZATION, HeaderValue};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::time::timeout;
use tokio_tungstenite::{
    WebSocketStream, connect_async,
    tungstenite::{client::IntoClientRequest, handshake::client::Request, protocol::Message},
};

use super::super::{BenchmarkSettings, RoundSample, Sample, metric_delta, response_is_complete};

pub(super) struct Case<'a> {
    pub(super) url: &'a str,
    pub(super) authorization: &'a str,
    pub(super) payloads: &'a [String],
    pub(super) metrics: Option<&'a crate::proxy::Metrics>,
}

#[derive(Debug, Default)]
struct ResponseTiming {
    first_event: Option<Duration>,
    response_events: u64,
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

async fn wait_for_response_complete<S>(
    socket: &mut WebSocketStream<S>,
    started: Instant,
) -> Result<ResponseTiming, Box<dyn Error>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut timing = ResponseTiming::default();
    while let Some(message) = socket.next().await {
        let message = message?;
        let complete = match &message {
            Message::Text(text) => response_is_complete(text.as_ref()),
            Message::Binary(bytes) => std::str::from_utf8(bytes).is_ok_and(response_is_complete),
            Message::Close(_) => {
                return Err(io::Error::other("WebSocket closed before response completion").into());
            }
            Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => false,
        };
        record_application_message(&mut timing, &message, started.elapsed());
        if complete {
            return Ok(timing);
        }
    }
    Err(io::Error::other("WebSocket ended before response completion").into())
}

fn authenticated_request(url: &str, authorization: &str) -> Result<Request, Box<dyn Error>> {
    let mut request = url.into_client_request()?;
    request.headers_mut().insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {authorization}"))?,
    );
    Ok(request)
}

async fn sample(case: &Case<'_>, settings: &BenchmarkSettings) -> Result<Sample, Box<dyn Error>> {
    let started = Instant::now();
    let request = authenticated_request(case.url, case.authorization)?;
    let (mut socket, response) = timeout(settings.timeout, connect_async(request)).await??;
    if response.status().as_u16() != 101 {
        return Err(io::Error::other(format!(
            "WebSocket benchmark response status {}",
            response.status()
        ))
        .into());
    }
    let setup = started.elapsed();
    let connection_started = Instant::now();
    let mut round_samples = Vec::with_capacity(case.payloads.len());
    for payload in case.payloads {
        let round_started = Instant::now();
        timeout(
            settings.timeout,
            socket.send(Message::Text(payload.to_owned().into())),
        )
        .await??;
        let timing = timeout(
            settings.timeout,
            wait_for_response_complete(&mut socket, round_started),
        )
        .await??;
        round_samples.push(RoundSample {
            e2e: round_started.elapsed(),
            first_event: timing.first_event,
            response_events: timing.response_events,
        });
    }
    timeout(settings.timeout, socket.close(None)).await??;
    let logical_requests = u64::try_from(case.payloads.len())?;
    Ok(Sample {
        e2e: started.elapsed(),
        setup,
        raw_bytes: u64::try_from(case.payloads.iter().map(String::len).sum::<usize>())?,
        encoded_bytes: 0,
        logical_requests,
        application_messages: logical_requests,
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
    })
}

pub(super) async fn collect_sample(
    case: &Case<'_>,
    settings: &BenchmarkSettings,
) -> Result<Sample, Box<dyn Error>> {
    let before = case
        .metrics
        .map(crate::proxy::Metrics::snapshot)
        .unwrap_or_default();
    let mut sample = sample(case, settings).await?;
    let (raw_bytes, encoded_bytes) = if let Some(metrics) = case.metrics {
        let after = metrics.snapshot();
        let (raw_bytes, encoded_bytes) = metric_delta(before, after, true);
        if raw_bytes == 0 || encoded_bytes == 0 {
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
        let messages = after
            .websocket_messages
            .saturating_sub(before.websocket_messages);
        if messages != sample.application_messages {
            return Err(io::Error::other(format!(
                "Turbo recorded {messages} WebSocket application messages for {} logical requests",
                sample.application_messages,
            ))
            .into());
        }
        sample.websocket_handshakes = handshakes;
        sample.websocket_reconnects = handshakes.saturating_sub(1);
        (raw_bytes, encoded_bytes)
    } else {
        let raw_bytes = u64::try_from(case.payloads.iter().map(String::len).sum::<usize>())?;
        (raw_bytes, raw_bytes)
    };
    if sample.first_events.len() != case.payloads.len() {
        return Err(io::Error::other("WebSocket response did not emit application data").into());
    }
    sample.raw_bytes = raw_bytes;
    sample.encoded_bytes = encoded_bytes;
    Ok(sample)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio_tungstenite::tungstenite::Message;

    use super::{ResponseTiming, authenticated_request, record_application_message};

    #[test]
    fn records_first_application_message_and_ignores_control_frames() {
        let mut timing = ResponseTiming::default();

        assert!(!record_application_message(
            &mut timing,
            &Message::Ping(vec![1].into()),
            Duration::from_millis(2),
        ));
        assert!(record_application_message(
            &mut timing,
            &Message::Text("first".into()),
            Duration::from_millis(4),
        ));
        assert!(record_application_message(
            &mut timing,
            &Message::Binary(vec![2].into()),
            Duration::from_millis(5),
        ));

        assert_eq!(timing.response_events, 2);
        assert_eq!(timing.first_event, Some(Duration::from_millis(4)));
    }

    #[test]
    fn standard_websocket_request_does_not_offer_turbo_or_deflate_extensions() {
        let request = authenticated_request("ws://127.0.0.1:1/v1/responses", "test-key")
            .expect("standard websocket request must build");

        assert!(request.headers().get("Sec-WebSocket-Extensions").is_none());
        assert!(request.headers().get("Sec-WebSocket-Protocol").is_none());
        assert_eq!(
            request
                .headers()
                .get("Authorization")
                .and_then(|value| value.to_str().ok()),
            Some("Bearer test-key")
        );
    }
}
