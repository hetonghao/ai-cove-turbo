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

use super::super::{BenchmarkCase, BenchmarkSettings, Sample, metric_delta};

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
    pub(super) name: &'static str,
    pub(super) client: &'a reqwest::Client,
    pub(super) url: &'a str,
    pub(super) authorization: &'a str,
    pub(super) payload: &'a str,
    pub(super) metrics: Option<&'a crate::proxy::Metrics>,
}

async fn sample(case: &Case<'_>, timeout_duration: Duration) -> Result<Sample, Box<dyn Error>> {
    let raw_bytes = u64::try_from(case.payload.len())?;
    let started = Instant::now();
    let completed_nanos = Arc::new(AtomicU64::new(0));
    let request_body = TimedBody {
        body: Some(Bytes::copy_from_slice(case.payload.as_bytes())),
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
        response.bytes().await.map(|_| status)
    })
    .await??;
    if !response.is_success() {
        return Err(io::Error::other(format!("HTTP benchmark response status {response}")).into());
    }
    let transport = completed_nanos.load(Ordering::Relaxed);
    if transport == 0 {
        return Err(io::Error::other("HTTP request body handoff was not observed").into());
    }
    Ok(Sample {
        e2e: started.elapsed(),
        transport: Duration::from_nanos(transport),
        setup: Duration::ZERO,
        raw_bytes,
        wire_bytes: raw_bytes,
    })
}

pub(super) async fn collect_case(
    case: Case<'_>,
    settings: &BenchmarkSettings,
) -> Result<BenchmarkCase, Box<dyn Error>> {
    let mut samples = Vec::with_capacity(settings.runs);
    for iteration in 0..settings.warmups + settings.runs {
        let before = case
            .metrics
            .map(crate::proxy::Metrics::snapshot)
            .unwrap_or_default();
        let mut sample = sample(&case, settings.timeout).await?;
        let after = case
            .metrics
            .map(crate::proxy::Metrics::snapshot)
            .unwrap_or_default();
        if case.metrics.is_some() {
            let (raw_bytes, wire_bytes) = metric_delta(before, after, false);
            if raw_bytes == 0 {
                return Err(io::Error::other("Turbo did not record the HTTP request").into());
            }
            sample.raw_bytes = raw_bytes;
            sample.wire_bytes = wire_bytes;
        }
        if iteration >= settings.warmups {
            samples.push(sample);
        }
    }
    Ok(BenchmarkCase {
        name: case.name,
        samples,
    })
}
