use super::*;

#[test]
fn reports_median_and_p95_without_reordering_samples() {
    let summary = summarize_latency(&[
        Duration::from_millis(10),
        Duration::from_millis(40),
        Duration::from_millis(20),
        Duration::from_millis(30),
        Duration::from_millis(50),
    ])
    .expect("non-empty latency samples must summarize");

    assert!((summary.median_ms - 30.0).abs() < 0.001);
    assert!((summary.p95_ms - 50.0).abs() < 0.001);
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
fn defines_short_long_and_multi_turn_workloads() {
    let settings = BenchmarkSettings {
        upstream: DEFAULT_UPSTREAM.to_owned(),
        model: DEFAULT_MODEL.to_owned(),
        prompt: DEFAULT_PROMPT_SEED.repeat(256),
        runs: 1,
        warmups: 0,
        timeout: DEFAULT_TIMEOUT,
    };

    let scenarios = usage_scenarios(&settings);

    assert_eq!(
        scenarios
            .iter()
            .map(|scenario| scenario.name)
            .collect::<Vec<_>>(),
        vec!["单轮短上下文", "单轮长上下文", "连续多轮会话"]
    );
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
}
