use std::{
    convert::Infallible,
    error::Error,
    io,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context, Poll},
    time::{Duration, Instant},
};

use bytes::Bytes;
use futures_util::Stream;
use reqwest::header::{AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, HeaderValue};
use tokio::time::timeout;

use super::super::{BenchmarkCase, BenchmarkSettings, RoundSample, Sample, metric_delta};

#[derive(Debug)]
struct TimedBody {
    body: Option<Bytes>,
    completed_nanos: Arc<AtomicU64>,
    started: Instant,
}

impl Stream for TimedBody {
    type Item = Result<Bytes, Infallible>;

    fn poll_next(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if let Some(body) = this.body.take() {
            let elapsed = this.started.elapsed().as_nanos();
            let elapsed = u64::try_from(elapsed).unwrap_or(u64::MAX);
            this.completed_nanos.store(elapsed, Ordering::Relaxed);
            return Poll::Ready(Some(Ok(body)));
        }
        Poll::Ready(None)
    }
}

pub(super) struct Case<'a> {
    pub(super) scenario: &'static str,
    pub(super) path: &'static str,
    pub(super) client: &'a reqwest::Client,
    pub(super) url: &'a str,
    pub(super) authorization: &'a str,
    pub(super) payloads: &'a [String],
    pub(super) metrics: Option<&'a crate::proxy::Metrics>,
}

fn response_event_count(body: &[u8]) -> u64 {
    let count = body
        .split(|byte| *byte == b'\n')
        .filter(|line| line.starts_with(b"data:"))
        .count();
    u64::try_from(count.max(1)).unwrap_or(u64::MAX)
}

async fn sample(
    case: &Case<'_>,
    payload: &str,
    timeout_duration: Duration,
) -> Result<RoundSample, Box<dyn Error>> {
    let raw_bytes = u64::try_from(payload.len())?;
    let started = Instant::now();
    let completed_nanos = Arc::new(AtomicU64::new(0));
    let request_body = TimedBody {
        body: Some(Bytes::copy_from_slice(payload.as_bytes())),
        completed_nanos: Arc::clone(&completed_nanos),
        started,
    };
    let authorization = HeaderValue::from_str(&format!("Bearer {}", case.authorization))?;
    let response = timeout(timeout_duration, async {
        let response = case
            .client
            .post(case.url)
            .header(AUTHORIZATION, authorization)
            .header(CONTENT_TYPE, "application/json")
            .header(CONTENT_LENGTH, raw_bytes)
            .body(reqwest::Body::wrap_stream(request_body))
            .send()
            .await?;
        let status = response.status();
        response.bytes().await.map(|body| (status, body))
    })
    .await??;
    let (status, body) = response;
    if !status.is_success() {
        return Err(io::Error::other(format!("HTTP benchmark response status {status}")).into());
    }
    let transport = completed_nanos.load(Ordering::Relaxed);
    if transport == 0 {
        return Err(io::Error::other("HTTP request body handoff was not observed").into());
    }
    Ok(RoundSample {
        e2e: started.elapsed(),
        transport: Duration::from_nanos(transport),
        response_events: response_event_count(&body),
    })
}

pub(super) async fn collect_case(
    case: Case<'_>,
    settings: &BenchmarkSettings,
) -> Result<BenchmarkCase, Box<dyn Error>> {
    let mut samples = Vec::with_capacity(settings.runs);
    for iteration in 0..settings.warmups + settings.runs {
        let started = Instant::now();
        let before = case
            .metrics
            .map(crate::proxy::Metrics::snapshot)
            .unwrap_or_default();
        let mut round_samples = Vec::with_capacity(case.payloads.len());
        for payload in case.payloads {
            round_samples.push(sample(&case, payload, settings.timeout).await?);
        }
        let after = case
            .metrics
            .map(crate::proxy::Metrics::snapshot)
            .unwrap_or_default();
        let logical_requests = u64::try_from(case.payloads.len())?;
        let (raw_bytes, wire_bytes) = if case.metrics.is_some() {
            let request_count = after.requests.saturating_sub(before.requests);
            if request_count != logical_requests {
                return Err(io::Error::other("Turbo did not record all HTTP requests").into());
            }
            metric_delta(before, after, false)
        } else {
            let raw_bytes = case.payloads.iter().map(String::len).sum::<usize>();
            let raw_bytes = u64::try_from(raw_bytes)?;
            (raw_bytes, raw_bytes)
        };
        if raw_bytes == 0 {
            return Err(io::Error::other("Turbo did not record all HTTP requests").into());
        }
        let transport = round_samples.iter().map(|sample| sample.transport).sum();
        let response_events = round_samples
            .iter()
            .map(|sample| sample.response_events)
            .sum();
        let sample = Sample {
            e2e: started.elapsed(),
            transport,
            setup: Duration::ZERO,
            raw_bytes,
            wire_bytes,
            logical_requests,
            application_messages: logical_requests,
            response_events,
            websocket_handshakes: 0,
            round_e2e: round_samples.iter().map(|sample| sample.e2e).collect(),
            round_transport: round_samples
                .iter()
                .map(|sample| sample.transport)
                .collect(),
        };
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

#[cfg(test)]
mod tests {
    use super::response_event_count;

    #[test]
    fn counts_sse_data_lines_and_keeps_non_stream_responses_visible() {
        assert_eq!(response_event_count(b"data: first\n\ndata: second\n\n"), 2);
        assert_eq!(response_event_count(br#"{"id":"response"}"#), 1);
    }
}
