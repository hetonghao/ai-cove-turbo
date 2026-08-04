use std::{error::Error, io, time::Instant};

use futures_util::StreamExt;
use reqwest::header::{AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, HeaderValue};
use tokio::time::timeout;

use super::super::{BenchmarkSettings, RoundSample, Sample, metric_delta};

pub(super) struct Case<'a> {
    pub(super) client: &'a reqwest::Client,
    pub(super) url: &'a str,
    pub(super) authorization: &'a str,
    pub(super) payloads: &'a [String],
    pub(super) metrics: Option<&'a crate::proxy::Metrics>,
}

#[derive(Debug, Default)]
struct SseResult {
    first_event: Option<std::time::Duration>,
    response_events: u64,
}

#[derive(Debug, Default)]
struct SseTracker {
    line: Vec<u8>,
    last_elapsed: std::time::Duration,
    result: SseResult,
}

impl SseTracker {
    fn push(&mut self, chunk: &[u8], elapsed: std::time::Duration) {
        self.last_elapsed = elapsed;
        for segment in chunk.split_inclusive(|byte| *byte == b'\n') {
            if let Some(line) = segment.strip_suffix(b"\n") {
                self.line.extend_from_slice(line);
                self.observe_line(elapsed);
                self.line.clear();
            } else {
                self.line.extend_from_slice(segment);
            }
        }
    }

    fn finish(mut self) -> SseResult {
        if !self.line.is_empty() {
            self.observe_line(self.last_elapsed);
        }
        self.result
    }

    fn observe_line(&mut self, elapsed: std::time::Duration) {
        let line = self.line.strip_suffix(b"\r").unwrap_or(&self.line);
        let Some(data) = line.strip_prefix(b"data:") else {
            return;
        };
        let data = data.strip_prefix(b" ").unwrap_or(data);
        if data.is_empty() {
            return;
        }
        self.result.response_events += 1;
        if self.result.first_event.is_none()
            && serde_json::from_slice::<serde_json::Value>(data).is_ok()
        {
            self.result.first_event = Some(elapsed);
        }
    }
}

fn response_event_count(body: &[u8]) -> u64 {
    let mut tracker = SseTracker::default();
    tracker.push(body, std::time::Duration::ZERO);
    tracker.finish().response_events.max(1)
}

async fn sample_round(
    case: &Case<'_>,
    payload: &str,
    settings: &BenchmarkSettings,
) -> Result<RoundSample, Box<dyn Error>> {
    let raw_bytes = u64::try_from(payload.len())?;
    let started = Instant::now();
    let authorization = HeaderValue::from_str(&format!("Bearer {}", case.authorization))?;
    let result = timeout(settings.timeout, async {
        let response = case
            .client
            .post(case.url)
            .header(AUTHORIZATION, authorization)
            .header(CONTENT_TYPE, "application/json")
            .header(CONTENT_LENGTH, raw_bytes)
            .body(payload.to_owned())
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            return Err(
                io::Error::other(format!("HTTP benchmark response status {status}")).into(),
            );
        }
        let mut body = response.bytes_stream();
        let mut tracker = SseTracker::default();
        while let Some(chunk) = body.next().await {
            let chunk = chunk?;
            tracker.push(&chunk, started.elapsed());
        }
        Ok::<SseResult, Box<dyn Error>>(tracker.finish())
    })
    .await??;
    Ok(RoundSample {
        e2e: started.elapsed(),
        first_event: result.first_event,
        response_events: result.response_events.max(1),
    })
}

pub(super) async fn collect_sample(
    case: &Case<'_>,
    settings: &BenchmarkSettings,
) -> Result<Sample, Box<dyn Error>> {
    let started = Instant::now();
    let before = case
        .metrics
        .map(crate::proxy::Metrics::snapshot)
        .unwrap_or_default();
    let mut round_samples = Vec::with_capacity(case.payloads.len());
    for payload in case.payloads {
        round_samples.push(sample_round(case, payload, settings).await?);
    }
    let after = case
        .metrics
        .map(crate::proxy::Metrics::snapshot)
        .unwrap_or_default();
    let logical_requests = u64::try_from(case.payloads.len())?;
    let (raw_bytes, encoded_bytes) = if case.metrics.is_some() {
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
    let first_events = round_samples
        .iter()
        .map(|sample| {
            sample
                .first_event
                .ok_or_else(|| io::Error::other("HTTP response did not emit a valid SSE event"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Sample {
        e2e: started.elapsed(),
        setup: std::time::Duration::ZERO,
        raw_bytes,
        encoded_bytes,
        logical_requests,
        application_messages: logical_requests,
        response_events: round_samples
            .iter()
            .map(|sample| sample.response_events)
            .sum(),
        websocket_handshakes: 0,
        round_e2e: round_samples.iter().map(|sample| sample.e2e).collect(),
        first_events,
        warm_round_e2e: round_samples
            .iter()
            .skip(1)
            .map(|sample| sample.e2e)
            .collect(),
        connection_lifetime: None,
        websocket_reconnects: 0,
        messages_per_connection: None,
    })
}

#[cfg(test)]
mod tests {
    use std::{convert::Infallible, time::Duration};

    use axum::{Router, body::Body, routing::post};
    use bytes::Bytes;
    use futures_util::StreamExt;

    use super::{Case, SseTracker, response_event_count, sample_round};
    use crate::benchmark::{BenchmarkSettings, settings::WorkloadSource};

    async fn stalled_sse_response() -> axum::response::Response {
        let stream = futures_util::stream::once(async {
            Ok::<_, Infallible>(Bytes::from_static(
                b"data: {\"type\":\"response.created\"}\n\n",
            ))
        })
        .chain(futures_util::stream::once(async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            Ok::<_, Infallible>(Bytes::from_static(
                b"data: {\"type\":\"response.completed\"}\n\n",
            ))
        }));
        axum::response::Response::new(Body::from_stream(stream))
    }

    #[test]
    fn tracks_first_valid_sse_event_across_chunks() {
        let mut tracker = SseTracker::default();
        tracker.push(
            b"data: [DONE]\n\ndata: {\"type\":\"response.output_",
            Duration::from_millis(10),
        );
        tracker.push(
            b"text.delta\"}\n\ndata: {\"type\":\"response.completed\"}\n",
            Duration::from_millis(20),
        );

        let result = tracker.finish();

        assert_eq!(result.response_events, 3);
        assert_eq!(result.first_event, Some(Duration::from_millis(20)));
    }

    #[test]
    fn counts_sse_data_lines_and_keeps_non_stream_responses_visible() {
        assert_eq!(response_event_count(b"data: first\n\ndata: second\n\n"), 2);
        assert_eq!(response_event_count(br#"{"id":"response"}"#), 1);
    }

    #[tokio::test]
    async fn times_out_while_draining_a_stalled_sse_body() -> Result<(), Box<dyn std::error::Error>>
    {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
        let address = listener.local_addr()?;
        let server = tokio::spawn(async move {
            let _ = axum::serve(
                listener,
                Router::new().route("/responses", post(stalled_sse_response)),
            )
            .await;
        });
        let client = reqwest::Client::new();
        let url = format!("http://{address}/responses");
        let settings = BenchmarkSettings {
            upstream: "http://127.0.0.1".to_owned(),
            model: "test-model".to_owned(),
            prompt: "test-prompt".to_owned(),
            workload_source: WorkloadSource::BuiltIn,
            runs: 4,
            warmups: 0,
            timeout: Duration::from_millis(10),
        };
        let case = Case {
            client: &client,
            url: &url,
            authorization: "test-key",
            payloads: &[],
            metrics: None,
        };

        let result = sample_round(&case, "{\"input\":\"test\"}", &settings).await;

        assert!(result.is_err());
        server.abort();
        Ok(())
    }
}
