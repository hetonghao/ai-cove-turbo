use std::{env, error::Error, io, time::Duration};

use crate::proxy::MetricsSnapshot;

mod live;
mod report;

#[cfg(test)]
mod tests;

const DEFAULT_UPSTREAM: &str = "https://api.ai-cove.com/v1";
const DEFAULT_MODEL: &str = "gpt-5.6-luna";
const DEFAULT_PROMPT_SEED: &str =
    "Turbo benchmark context: keep this context unchanged and reply with OK only.\n";
const DEFAULT_SHORT_PROMPT: &str = "Reply with OK only.";
const DEFAULT_MULTI_ROUNDS: usize = 5;
const DEFAULT_RUNS: usize = 5;
const DEFAULT_WARMUPS: usize = 1;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Debug)]
struct BenchmarkSettings {
    upstream: String,
    model: String,
    prompt: String,
    runs: usize,
    warmups: usize,
    timeout: Duration,
}

#[derive(Debug)]
struct UsageScenario {
    name: &'static str,
    prompts: Vec<String>,
}

#[derive(Debug)]
struct RoundSample {
    e2e: Duration,
    transport: Duration,
    response_events: u64,
}

#[derive(Debug)]
struct Sample {
    e2e: Duration,
    transport: Duration,
    setup: Duration,
    raw_bytes: u64,
    wire_bytes: u64,
    logical_requests: u64,
    application_messages: u64,
    response_events: u64,
    websocket_handshakes: u64,
    round_e2e: Vec<Duration>,
    round_transport: Vec<Duration>,
}

#[derive(Debug)]
struct BenchmarkCase {
    scenario: &'static str,
    path: &'static str,
    samples: Vec<Sample>,
}

#[derive(Clone, Copy, Debug)]
struct LatencySummary {
    median_ms: f64,
    p95_ms: f64,
}

impl BenchmarkSettings {
    fn from_env() -> Result<Self, Box<dyn Error>> {
        Ok(Self {
            upstream: env::var("TURBO_BENCHMARK_UPSTREAM")
                .unwrap_or_else(|_| DEFAULT_UPSTREAM.to_owned()),
            model: env::var("TURBO_BENCHMARK_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_owned()),
            prompt: env::var("TURBO_BENCHMARK_PROMPT")
                .unwrap_or_else(|_| DEFAULT_PROMPT_SEED.repeat(256)),
            runs: positive_env("TURBO_BENCHMARK_RUNS", DEFAULT_RUNS)?,
            warmups: non_negative_env("TURBO_BENCHMARK_WARMUPS", DEFAULT_WARMUPS)?,
            timeout: Duration::from_secs(positive_env_u64(
                "TURBO_BENCHMARK_TIMEOUT_SECS",
                DEFAULT_TIMEOUT.as_secs(),
            )?),
        })
    }
}

fn positive_env(name: &str, default: usize) -> Result<usize, Box<dyn Error>> {
    let value = env::var(name).unwrap_or_else(|_| default.to_string());
    let parsed = value.parse::<usize>()?;
    if parsed == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{name} must be positive"),
        )
        .into());
    }
    Ok(parsed)
}

fn positive_env_u64(name: &str, default: u64) -> Result<u64, Box<dyn Error>> {
    let value = env::var(name).unwrap_or_else(|_| default.to_string());
    let parsed = value.parse::<u64>()?;
    if parsed == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{name} must be positive"),
        )
        .into());
    }
    Ok(parsed)
}

fn non_negative_env(name: &str, default: usize) -> Result<usize, Box<dyn Error>> {
    Ok(env::var(name)
        .unwrap_or_else(|_| default.to_string())
        .parse::<usize>()?)
}

fn usage_scenarios(settings: &BenchmarkSettings) -> Vec<UsageScenario> {
    let multi_turn_prompts = (1..=DEFAULT_MULTI_ROUNDS)
        .map(|round| {
            format!(
                "Turbo benchmark multi-turn round {round}; keep the context unchanged and reply with OK only.\n{}",
                settings.prompt
            )
        })
        .collect();
    vec![
        UsageScenario {
            name: "单轮短上下文",
            prompts: vec![DEFAULT_SHORT_PROMPT.to_owned()],
        },
        UsageScenario {
            name: "单轮长上下文",
            prompts: vec![settings.prompt.clone()],
        },
        UsageScenario {
            name: "连续多轮会话",
            prompts: multi_turn_prompts,
        },
    ]
}

fn http_payload(model: &str, prompt: &str) -> String {
    serde_json::json!({
        "model": model,
        "input": prompt,
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

fn summarize_latency(values: &[Duration]) -> Option<LatencySummary> {
    let mut sorted = values.iter().map(Duration::as_secs_f64).collect::<Vec<_>>();
    if sorted.is_empty() {
        return None;
    }
    sorted.sort_by(f64::total_cmp);
    Some(LatencySummary {
        median_ms: percentile_ms(&sorted, 50)?,
        p95_ms: percentile_ms(&sorted, 95)?,
    })
}

fn percentile_ms(sorted: &[f64], percentile: usize) -> Option<f64> {
    let rank = sorted
        .len()
        .checked_mul(percentile)?
        .div_ceil(100)
        .saturating_sub(1);
    sorted.get(rank).copied().map(|seconds| seconds * 1000.0)
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
