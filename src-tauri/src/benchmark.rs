use std::{error::Error, io, time::Duration};

use crate::proxy::MetricsSnapshot;

mod live;
mod report;
mod settings;

use settings::{
    BenchmarkSettings, DEFAULT_MODEL, DEFAULT_MULTI_ROUNDS, DEFAULT_TIMEOUT, DEFAULT_UPSTREAM,
    UsageScenario, default_long_prompt, usage_scenarios, workload_fingerprint,
};

const DIRECT_PATH: &str = "直连（不走 Turbo）";
const HTTP_PATH: &str = "Turbo HTTP + 自适应 zstd";
const WEBSOCKET_PATH: &str = "Turbo WS + 自适应 zstd";
const BENCHMARK_INSTRUCTIONS: &str = "Treat the input as context and reply with OK only.";

#[cfg(test)]
mod tests;

#[derive(Debug)]
struct RoundSample {
    e2e: Duration,
    first_event: Option<Duration>,
    response_events: u64,
}

#[derive(Debug)]
struct Sample {
    e2e: Duration,
    setup: Duration,
    raw_bytes: u64,
    encoded_bytes: u64,
    logical_requests: u64,
    application_messages: u64,
    response_events: u64,
    websocket_handshakes: u64,
    round_e2e: Vec<Duration>,
    first_events: Vec<Duration>,
    warm_round_e2e: Vec<Duration>,
    connection_lifetime: Option<Duration>,
    websocket_reconnects: u64,
    messages_per_connection: Option<u64>,
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

fn response_is_complete(event: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(event)
        .ok()
        .and_then(|value| {
            value
                .get("type")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .is_some_and(|kind| matches!(kind.as_str(), "response.completed" | "response.done"))
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

fn responses_url(base: &str, websocket: bool) -> Result<String, Box<dyn Error>> {
    let mut url = url::Url::parse(base)?;
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
                )
                .into());
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
