use super::*;
use std::io::Cursor;

#[test]
fn reports_median_and_exact_range_without_reordering_samples() {
    let summary = summarize_latency(&[
        Duration::from_millis(10),
        Duration::from_millis(40),
        Duration::from_millis(20),
        Duration::from_millis(30),
        Duration::from_millis(50),
    ])
    .expect("non-empty latency samples must summarize");

    assert!((summary.median - 30.0).abs() < 0.001);
    assert!((summary.min - 10.0).abs() < 0.001);
    assert!((summary.max - 50.0).abs() < 0.001);
}

#[test]
fn recognizes_responses_completion_events_only() {
    assert!(response_is_complete(r#"{"type":"response.completed"}"#));
    assert!(response_is_complete(r#"{"type":"response.done"}"#));
    assert!(!response_is_complete(
        r#"{"type":"response.output_text.delta"}"#
    ));
    assert!(!response_is_complete("[DONE]"));
}

#[test]
fn builds_http_and_websocket_responses_urls_from_the_same_base() {
    assert_eq!(
        responses_url("https://api.ai-cove.com/v1", false).expect("valid HTTP URL"),
        "https://api.ai-cove.com/v1/responses"
    );
    assert_eq!(
        responses_url("http://127.0.0.1:44175/v1", true).expect("valid loopback URL"),
        "ws://127.0.0.1:44175/v1/responses"
    );
}

#[test]
fn defines_short_long_and_reused_connection_workloads() {
    let settings = BenchmarkSettings {
        upstream: DEFAULT_UPSTREAM.to_owned(),
        model: DEFAULT_MODEL.to_owned(),
        prompt: default_long_prompt(),
        workload_source: super::settings::WorkloadSource::BuiltIn,
        runs: 1,
        warmups: 0,
        timeout: DEFAULT_TIMEOUT,
    };

    let scenarios = usage_scenarios(&settings);

    assert_eq!(scenarios.len(), 3);
    let short = scenarios.first().expect("short scenario must exist");
    let long = scenarios.get(1).expect("long scenario must exist");
    let multi = scenarios.get(2).expect("multi-turn scenario must exist");
    assert_eq!(short.prompts.len(), 1);
    assert!(
        short
            .prompts
            .first()
            .expect("short prompt must exist")
            .len()
            < long.prompts.first().expect("long prompt must exist").len()
    );
    assert_eq!(multi.prompts.len(), DEFAULT_MULTI_ROUNDS);
    assert!(!short.requires_compression);
    assert!(long.requires_compression);
    assert!(multi.requires_compression);
}

#[test]
fn reports_payload_growth_as_negative_reduction() {
    assert!((super::report::payload_reduction_pct(102, 112) + 9.803_921).abs() < 0.000_001);
    assert!((super::report::payload_reduction_pct(20_040, 138) - 99.311_377).abs() < 0.000_001);
}

#[test]
fn default_long_workload_avoids_extreme_repetition() {
    let prompt = default_long_prompt();
    let payload = http_payload(DEFAULT_MODEL, &prompt);
    let encoded = zstd::stream::encode_all(Cursor::new(payload.as_bytes()), 3)
        .expect("benchmark fixture must compress");
    let reduction = super::report::payload_reduction_pct(
        u64::try_from(payload.len()).expect("payload fits u64"),
        u64::try_from(encoded.len()).expect("payload fits u64"),
    );

    assert!((30.0..90.0).contains(&reduction));
    assert!(!prompt.is_empty());
}

#[test]
fn workload_fingerprint_changes_with_input() {
    assert_ne!(
        workload_fingerprint(b"fixture-a"),
        workload_fingerprint(b"fixture-b")
    );
    assert_eq!(
        workload_fingerprint(b"fixture-a"),
        workload_fingerprint(b"fixture-a")
    );
}

#[test]
fn computes_payload_serialization_time_without_claiming_network_latency() {
    assert!((super::report::payload_serialization_ms(1_250_000, 10.0) - 1_000.0).abs() < 0.001);
    assert!(super::report::payload_serialization_ms(10, 0.0).abs() < f64::EPSILON);
}

#[test]
fn http_and_websocket_payloads_share_the_same_workload_contract() {
    let http: serde_json::Value =
        serde_json::from_str(&http_payload("model", "input")).expect("HTTP payload must be JSON");
    let websocket: serde_json::Value = serde_json::from_str(&websocket_payload("model", "input"))
        .expect("WebSocket payload must be JSON");

    for payload in [&http, &websocket] {
        assert_eq!(
            payload.get("model").and_then(serde_json::Value::as_str),
            Some("model")
        );
        assert_eq!(
            payload.get("input").and_then(serde_json::Value::as_str),
            Some("input")
        );
        assert_eq!(
            payload
                .get("max_output_tokens")
                .and_then(serde_json::Value::as_u64),
            Some(16)
        );
        assert!(
            payload
                .get("instructions")
                .and_then(serde_json::Value::as_str)
                .is_some()
        );
    }
    assert_eq!(
        http.get("stream").and_then(serde_json::Value::as_bool),
        Some(true)
    );
    assert_eq!(
        websocket.get("type").and_then(serde_json::Value::as_str),
        Some("response.create")
    );
}

#[test]
fn requires_complete_three_path_rotation_cycles() {
    assert_eq!(
        super::settings::validate_runs(3).expect("one cycle is valid"),
        3
    );
    assert_eq!(
        super::settings::validate_runs(12).expect("default-sized cycle count is valid"),
        12
    );
    assert!(super::settings::validate_runs(4).is_err());
    assert!(super::settings::validate_runs(1).is_err());
    assert!(super::settings::validate_runs(10).is_err());
}
