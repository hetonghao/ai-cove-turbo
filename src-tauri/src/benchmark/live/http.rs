use std::{io, time::Instant};

use futures_util::StreamExt;
use reqwest::header::{AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, HeaderValue};
use tokio::time::timeout;

use super::super::{
    BenchmarkResult, BenchmarkSettings, Completion, RoundSample, RoundTransport, Sample,
    benchmark_error, completion_response_id, metric_delta, payload_with_previous_response_id,
};

fn round_error(round: usize, error: &io::Error) -> io::Error {
    io::Error::new(
        error.kind(),
        format!("HTTP benchmark round {round} failed: {error}"),
    )
}

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
    response_id: Option<String>,
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
        if let Completion::Complete(response_id) = completion_response_id(data) {
            self.result.response_id = response_id;
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
) -> BenchmarkResult<RoundSample> {
    let raw_bytes = u64::try_from(payload.len()).map_err(benchmark_error)?;
    let started = Instant::now();
    let authorization = HeaderValue::from_str(&format!("Bearer {}", case.authorization))
        .map_err(benchmark_error)?;
    let result = timeout(settings.timeout, async {
        let response = case
            .client
            .post(case.url)
            .header(AUTHORIZATION, authorization)
            .header(CONTENT_TYPE, "application/json")
            .header(CONTENT_LENGTH, raw_bytes)
            .body(payload.to_owned())
            .send()
            .await
            .map_err(benchmark_error)?;
        let status = response.status();
        if !status.is_success() {
            let kind =
                if status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
                    io::ErrorKind::ConnectionAborted
                } else {
                    io::ErrorKind::Other
                };
            return Err(io::Error::new(
                kind,
                format!("HTTP benchmark response status {status}"),
            ));
        }
        let mut body = response.bytes_stream();
        let mut tracker = SseTracker::default();
        while let Some(chunk) = body.next().await {
            let chunk = chunk.map_err(benchmark_error)?;
            tracker.push(&chunk, started.elapsed());
        }
        Ok::<SseResult, io::Error>(tracker.finish())
    })
    .await
    .map_err(benchmark_error)??;
    Ok(RoundSample {
        e2e: started.elapsed(),
        first_event: result.first_event,
        response_events: result.response_events.max(1),
        response_id: result.response_id,
        request_bytes: raw_bytes,
    })
}

pub(super) async fn collect_sample(
    case: &Case<'_>,
    settings: &BenchmarkSettings,
) -> BenchmarkResult<Sample> {
    let started = Instant::now();
    let before = case
        .metrics
        .map(crate::proxy::Metrics::snapshot)
        .unwrap_or_default();
    let mut round_samples = Vec::with_capacity(case.payloads.len());
    let mut previous_response_id = None;
    for (round, payload) in case.payloads.iter().enumerate() {
        let payload = payload_with_previous_response_id(payload, previous_response_id.as_deref())?;
        let sample = sample_round(case, &payload, settings)
            .await
            .map_err(|error| round_error(round + 1, &error))?;
        if round + 1 < case.payloads.len() && sample.response_id.is_none() {
            return Err(round_error(
                round + 1,
                &io::Error::other("completed response did not include response.id"),
            ));
        }
        previous_response_id.clone_from(&sample.response_id);
        round_samples.push(sample);
    }
    let after = case
        .metrics
        .map(crate::proxy::Metrics::snapshot)
        .unwrap_or_default();
    let logical_requests = u64::try_from(case.payloads.len()).map_err(benchmark_error)?;
    let http_requests = after.requests.saturating_sub(before.requests);
    let (raw_bytes, encoded_bytes) = if case.metrics.is_some() {
        if http_requests != logical_requests {
            return Err(io::Error::other("Turbo did not record all HTTP requests"));
        }
        metric_delta(before, after, false)
    } else {
        let raw_bytes = round_samples.iter().try_fold(0_u64, |total, sample| {
            total
                .checked_add(sample.request_bytes)
                .ok_or_else(|| io::Error::other("HTTP benchmark byte count overflow"))
        })?;
        (raw_bytes, raw_bytes)
    };
    if raw_bytes == 0 {
        return Err(io::Error::other("Turbo did not record all HTTP requests"));
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
        http_requests: if case.metrics.is_some() {
            http_requests
        } else {
            logical_requests
        },
        websocket_messages: 0,
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
        retries: 0,
        round_transports: vec![RoundTransport::Http; case.payloads.len()],
    })
}

#[cfg(test)]
mod tests {
    use std::{
        convert::Infallible,
        sync::{Arc, Mutex},
        time::Duration,
    };

    use axum::{Json, Router, body::Body, extract::State, http::StatusCode, routing::post};
    use bytes::Bytes;
    use futures_util::StreamExt;

    use super::{
        Case, SseTracker, collect_sample, response_event_count, round_error, sample_round,
    };
    use crate::benchmark::{
        BenchmarkResult, BenchmarkSettings, http_payload, settings::WorkloadSource,
    };

    type Requests = Arc<Mutex<Vec<serde_json::Value>>>;

    async fn chained_response(
        State(requests): State<Requests>,
        Json(payload): Json<serde_json::Value>,
    ) -> Result<String, StatusCode> {
        let mut requests = requests
            .lock()
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        requests.push(payload);
        let response_id = format!("resp-{}", requests.len());
        drop(requests);
        Ok(format!(
            "data: {{\"type\":\"response.completed\",\"response\":{{\"id\":\"{response_id}\"}}}}\n\n"
        ))
    }

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

    #[test]
    fn reports_http_round_for_sample_failures() {
        let cause = std::io::Error::other("HTTP 524");
        let error = round_error(3, &cause);
        let message = error.to_string();

        assert!(message.contains("HTTP benchmark round 3 failed"));
        assert!(message.contains("HTTP 524"));
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

    #[tokio::test]
    async fn chains_each_http_round_to_the_previous_response() -> BenchmarkResult<()> {
        let requests = Requests::default();
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
        let address = listener.local_addr()?;
        let server_requests = Arc::clone(&requests);
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/responses", post(chained_response))
                    .with_state(server_requests),
            )
            .await
        });
        let client = reqwest::Client::new();
        let url = format!("http://{address}/responses");
        let payloads = [
            http_payload("test-model", "first"),
            http_payload("test-model", "second"),
            http_payload("test-model", "third"),
        ];
        let settings = BenchmarkSettings {
            upstream: format!("http://{address}"),
            model: "test-model".to_owned(),
            prompt: "test-prompt".to_owned(),
            workload_source: WorkloadSource::BuiltIn,
            runs: 4,
            warmups: 0,
            timeout: Duration::from_secs(1),
        };

        let sample = collect_sample(
            &Case {
                client: &client,
                url: &url,
                authorization: "test-key",
                payloads: &payloads,
                metrics: None,
            },
            &settings,
        )
        .await?;
        let template_bytes = u64::try_from(payloads.iter().map(String::len).sum::<usize>())
            .map_err(crate::benchmark::benchmark_error)?;
        let requests = requests
            .lock()
            .map_err(|_| std::io::Error::other("request fixture lock poisoned"))?;
        let first = requests
            .first()
            .ok_or_else(|| std::io::Error::other("missing first request"))?;
        let second = requests
            .get(1)
            .ok_or_else(|| std::io::Error::other("missing second request"))?;
        let third = requests
            .get(2)
            .ok_or_else(|| std::io::Error::other("missing third request"))?;

        assert!(sample.raw_bytes > template_bytes);
        assert!(first.get("previous_response_id").is_none());
        assert_eq!(
            second
                .get("previous_response_id")
                .and_then(serde_json::Value::as_str),
            Some("resp-1")
        );
        assert_eq!(
            third
                .get("previous_response_id")
                .and_then(serde_json::Value::as_str),
            Some("resp-2")
        );
        drop(requests);
        server.abort();
        Ok(())
    }
}
