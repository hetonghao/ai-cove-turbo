use std::{error::Error, io, time::Duration};

use crate::proxy::MetricsSnapshot;

mod calibration;
mod live;
mod report;
mod settings;
mod stability;

pub(super) type BenchmarkResult<T> = Result<T, io::Error>;

pub(super) fn benchmark_error<E>(error: E) -> io::Error
where
    E: Error + Send + Sync + 'static,
{
    io::Error::other(error)
}

use settings::{
    BenchmarkSettings, DEFAULT_MODEL, DEFAULT_MULTI_ROUNDS, DEFAULT_TIMEOUT, DEFAULT_UPSTREAM,
    UsageScenario, default_long_prompt, usage_scenarios, workload_fingerprint,
};

const DIRECT_PATH: &str = "直连（不走 Turbo）";
const HTTP_PATH: &str = "Turbo HTTP + 自适应 zstd";
const WEBSOCKET_PATH: &str = "Turbo WS + 自适应 zstd";
const HYBRID_PATH: &str = "local-WS Hybrid";
const BENCHMARK_INSTRUCTIONS: &str = "Treat the input as context and reply with OK only.";

#[cfg(test)]
mod tests;

#[derive(Debug)]
struct RoundSample {
    e2e: Duration,
    first_event: Option<Duration>,
    response_events: u64,
    response_id: Option<String>,
    request_bytes: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RoundTransport {
    Http,
    WebSocket,
}

impl RoundTransport {
    const fn label(self) -> &'static str {
        match self {
            Self::Http => "HTTP",
            Self::WebSocket => "WS",
        }
    }
}

#[derive(Debug)]
struct Sample {
    e2e: Duration,
    setup: Duration,
    raw_bytes: u64,
    encoded_bytes: u64,
    logical_requests: u64,
    application_messages: u64,
    http_requests: u64,
    websocket_messages: u64,
    response_events: u64,
    websocket_handshakes: u64,
    round_e2e: Vec<Duration>,
    first_events: Vec<Duration>,
    warm_round_e2e: Vec<Duration>,
    connection_lifetime: Option<Duration>,
    websocket_reconnects: u64,
    messages_per_connection: Option<u64>,
    retries: u64,
    round_transports: Vec<RoundTransport>,
    compression_metrics: Option<CompressionSampleMetrics>,
}

#[derive(Clone, Copy, Debug, Default)]
struct CompressionSampleMetrics {
    encode_count: u64,
    decode_count: u64,
    queue_wait_ms: u64,
    work_time_ms: u64,
    failures: u64,
    fast_path_count: u64,
}

fn compression_metric_delta(
    before: MetricsSnapshot,
    after: MetricsSnapshot,
) -> CompressionSampleMetrics {
    CompressionSampleMetrics {
        encode_count: after
            .compression_encode_count
            .saturating_sub(before.compression_encode_count),
        decode_count: after
            .compression_decode_count
            .saturating_sub(before.compression_decode_count),
        queue_wait_ms: after
            .compression_queue_wait_ms
            .saturating_sub(before.compression_queue_wait_ms),
        work_time_ms: after
            .compression_work_time_ms
            .saturating_sub(before.compression_work_time_ms),
        failures: after
            .compression_failures
            .saturating_sub(before.compression_failures),
        fast_path_count: after
            .compression_fast_path_count
            .saturating_sub(before.compression_fast_path_count),
    }
}

#[derive(Debug)]
struct BenchmarkCase {
    scenario: &'static str,
    path: &'static str,
    samples: Vec<Sample>,
}

#[derive(Clone, Copy, Debug)]
struct LatencyMsSummary {
    median: f64,
    min: f64,
    max: f64,
}

fn http_payload(model: &str, prompt: &str) -> String {
    serde_json::json!({
        "model": model,
        "input": prompt,
        "instructions": BENCHMARK_INSTRUCTIONS,
        "stream": true,
        "max_output_tokens": 16,
    })
    .to_string()
}

fn websocket_payload(model: &str, prompt: &str) -> String {
    serde_json::json!({
        "type": "response.create",
        "model": model,
        "input": prompt,
        "instructions": BENCHMARK_INSTRUCTIONS,
        "max_output_tokens": 16,
    })
    .to_string()
}

fn payload_with_previous_response_id(
    payload: &str,
    previous_response_id: Option<&str>,
) -> BenchmarkResult<String> {
    let mut value = serde_json::from_str::<serde_json::Value>(payload).map_err(benchmark_error)?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| io::Error::other("benchmark payload must be a JSON object"))?;
    object.remove("previous_response_id");
    if let Some(response_id) = previous_response_id {
        object.insert(
            "previous_response_id".to_owned(),
            serde_json::Value::String(response_id.to_owned()),
        );
    }
    serde_json::to_string(&value).map_err(benchmark_error)
}

enum Completion {
    Pending,
    Complete(Option<String>),
    Failed(String),
}

fn completion_response_id(event: &[u8]) -> Completion {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(event) else {
        return Completion::Pending;
    };
    let Some(event_type) = value.get("type").and_then(serde_json::Value::as_str) else {
        return Completion::Pending;
    };
    if matches!(
        event_type,
        "response.failed"
            | "response.incomplete"
            | "response.cancelled"
            | "response.canceled"
            | "error"
    ) {
        let error = value.get("error").or_else(|| {
            value
                .get("response")
                .and_then(|response| response.get("error"))
        });
        let code = error
            .and_then(|error| error.get("code"))
            .and_then(serde_json::Value::as_str);
        let message = error
            .and_then(|error| error.get("message"))
            .and_then(serde_json::Value::as_str);
        return Completion::Failed(match (code, message) {
            (Some(code), Some(message)) => {
                format!("{event_type}: code={code}, message={message}")
            }
            (Some(code), None) => format!("{event_type}: code={code}"),
            (None, Some(message)) => format!("{event_type}: message={message}"),
            (None, None) => event_type.to_owned(),
        });
    }
    if !matches!(event_type, "response.completed" | "response.done") {
        return Completion::Pending;
    }
    Completion::Complete(
        value
            .get("response")
            .and_then(|response| response.get("id"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
    )
}

fn response_failure_error(failure: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::ConnectionAborted,
        format!("upstream response ended with {failure}"),
    )
}

fn response_is_complete(event: &str) -> bool {
    matches!(
        completion_response_id(event.as_bytes()),
        Completion::Complete(_)
    )
}

fn summarize_latency(values: &[Duration]) -> Option<LatencyMsSummary> {
    let mut sorted = values.iter().map(Duration::as_secs_f64).collect::<Vec<_>>();
    if sorted.is_empty() {
        return None;
    }
    sorted.sort_by(f64::total_cmp);
    let middle = sorted.len() / 2;
    let upper = sorted.get(middle).copied()?;
    let median = if sorted.len().is_multiple_of(2) {
        let lower = sorted.get(middle.checked_sub(1)?).copied()?;
        lower.midpoint(upper)
    } else {
        upper
    };
    Some(LatencyMsSummary {
        median: median * 1000.0,
        min: sorted.first().copied()? * 1000.0,
        max: sorted.last().copied()? * 1000.0,
    })
}

fn responses_url(base: &str, websocket: bool) -> BenchmarkResult<String> {
    let mut url = url::Url::parse(base).map_err(benchmark_error)?;
    let path = format!("{}/responses", url.path().trim_end_matches('/'));
    url.set_path(&path);
    url.set_query(None);
    let scheme = if websocket {
        match url.scheme() {
            "https" => "wss".to_owned(),
            "http" => "ws".to_owned(),
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "upstream must use http(s)",
                ));
            }
        }
    } else {
        url.scheme().to_owned()
    };
    url.set_scheme(&scheme)
        .map_err(|()| io::Error::new(io::ErrorKind::InvalidInput, "invalid upstream scheme"))?;
    Ok(url.to_string())
}

fn metric_delta(before: MetricsSnapshot, after: MetricsSnapshot, websocket: bool) -> (u64, u64) {
    if websocket {
        (
            after
                .websocket_raw_bytes
                .saturating_sub(before.websocket_raw_bytes),
            after
                .websocket_sent_bytes
                .saturating_sub(before.websocket_sent_bytes),
        )
    } else {
        (
            after.raw_bytes.saturating_sub(before.raw_bytes),
            after.sent_bytes.saturating_sub(before.sent_bytes),
        )
    }
}
