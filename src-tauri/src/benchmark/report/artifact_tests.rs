use std::time::Duration;

use super::super::super::{
    BenchmarkCase, BenchmarkSettings, DIRECT_PATH, HTTP_PATH, RoundTransport, Sample,
};

fn settings(prompt: &str) -> BenchmarkSettings {
    BenchmarkSettings {
        upstream: "https://example.invalid/v1".to_owned(),
        model: "fixture-model".to_owned(),
        prompt: prompt.to_owned(),
        workload_source: super::super::super::settings::WorkloadSource::File,
        runs: 4,
        warmups: 1,
        timeout: Duration::from_secs(1),
    }
}

fn sample(e2e_ms: u64) -> Sample {
    Sample {
        e2e: Duration::from_millis(e2e_ms),
        setup: Duration::from_millis(5),
        raw_bytes: 2_048,
        encoded_bytes: 1_024,
        logical_requests: 1,
        application_messages: 1,
        http_requests: 1,
        websocket_messages: 0,
        response_events: 2,
        websocket_handshakes: 0,
        round_e2e: vec![Duration::from_millis(e2e_ms)],
        first_events: vec![Duration::from_millis(e2e_ms / 2)],
        warm_round_e2e: Vec::new(),
        connection_lifetime: None,
        websocket_reconnects: 0,
        messages_per_connection: None,
        retries: 0,
        round_transports: vec![RoundTransport::Http],
        compression_metrics: None,
    }
}

fn cases() -> Vec<BenchmarkCase> {
    vec![
        BenchmarkCase {
            scenario: "fixture",
            path: DIRECT_PATH,
            samples: vec![sample(100), sample(110)],
        },
        BenchmarkCase {
            scenario: "fixture",
            path: HTTP_PATH,
            samples: vec![sample(90), sample(95)],
        },
    ]
}

#[path = "artifact_tests_comparison.rs"]
mod comparison;
#[path = "artifact_tests_shape.rs"]
mod shape;
