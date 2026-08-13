use std::{env, io, path::PathBuf, time::Duration};

use tokio::{sync::OnceCell, time::timeout};
use url::Url;

use super::{
    BenchmarkPath, BenchmarkResult, LiveContext, PayloadSet, benchmark_error, collect_scenario,
    disable_path,
};
use crate::benchmark::{
    BenchmarkSettings, DEFAULT_MULTI_ROUNDS, calibration::generate_candidate_artifacts,
    responses_url, usage_scenarios, websocket_payload,
};

const WORKLOAD_PROFILE_ENV: &str = "TURBO_BENCHMARK_WORKLOAD_PROFILE";
const CANDIDATE_OUTPUT_ENV: &str = "TURBO_BENCHMARK_CANDIDATE_OUTPUT";

fn write_calibration_if_requested(
    settings: &BenchmarkSettings,
    cases: &[crate::benchmark::BenchmarkCase],
) -> BenchmarkResult<()> {
    match (
        env::var_os(WORKLOAD_PROFILE_ENV),
        env::var_os(CANDIDATE_OUTPUT_ENV),
    ) {
        (None, None) => Ok(()),
        (Some(profile), Some(output)) => {
            let stdout = io::stdout();
            generate_candidate_artifacts(
                &mut stdout.lock(),
                &PathBuf::from(profile),
                &PathBuf::from(output),
                settings,
                cases,
            )
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{WORKLOAD_PROFILE_ENV} and {CANDIDATE_OUTPUT_ENV} must be set together"),
        )),
    }
}

pub(super) async fn verify_benchmark_websocket_lifecycle(
    context: &LiveContext<'_>,
    path: BenchmarkPath,
) -> BenchmarkResult<()> {
    let scenario = usage_scenarios(context.settings)
        .into_iter()
        .find(|scenario| scenario.prompts.len() == DEFAULT_MULTI_ROUNDS)
        .ok_or_else(|| io::Error::other("multi-turn WebSocket gate scenario is missing"))?;
    let websocket_payloads = scenario
        .prompts
        .iter()
        .map(|prompt| websocket_payload(&context.settings.model, prompt))
        .collect::<Vec<_>>();
    let payloads = PayloadSet {
        http: &[],
        websocket: &websocket_payloads,
    };
    let sample = context
        .collect_sample(path, &payloads)
        .await
        .map_err(|error| {
            io::Error::other(format!("{} lifecycle sample failed: {error}", path.label()))
        })?;
    let expected_messages = u64::try_from(websocket_payloads.len()).map_err(benchmark_error)?;
    match path {
        BenchmarkPath::WebSocket
            if sample.websocket_handshakes == 1
                && sample.application_messages == expected_messages
                && sample.websocket_messages == expected_messages
                && sample.messages_per_connection == Some(expected_messages)
                && sample.websocket_reconnects == 0 => {}
        BenchmarkPath::Hybrid
            if sample.http_requests == 0
                && sample.websocket_messages == expected_messages
                && sample.messages_per_connection == Some(expected_messages)
                && sample.websocket_reconnects == 0
                && sample
                    .round_transports
                    .iter()
                    .all(|transport| *transport == crate::benchmark::RoundTransport::WebSocket) => {
        }
        BenchmarkPath::WebSocket
        | BenchmarkPath::Hybrid
        | BenchmarkPath::Direct
        | BenchmarkPath::Http => {
            return Err(io::Error::other(format!(
                "benchmark-client {} lifecycle gate failed: handshakes={}, logical_requests={}, http_requests={}, websocket_messages={}, messages_per_connection={:?}, reconnects={}",
                path.label(),
                sample.websocket_handshakes,
                sample.logical_requests,
                sample.http_requests,
                sample.websocket_messages,
                sample.messages_per_connection,
                sample.websocket_reconnects,
            )));
        }
    }
    Ok(())
}

fn validate_cold_hybrid_sample(sample: &crate::benchmark::Sample) -> BenchmarkResult<()> {
    if sample.logical_requests == 1
        && sample.http_requests == 1
        && sample.websocket_messages == 0
        && sample.retries == 0
        && sample.round_transports == [crate::benchmark::RoundTransport::Http]
    {
        return Ok(());
    }
    Err(io::Error::other(format!(
        "Hybrid cold evidence requires one retry-free HTTP request: logical_requests={}, http_requests={}, websocket_messages={}, retries={}",
        sample.logical_requests, sample.http_requests, sample.websocket_messages, sample.retries,
    )))
}

async fn prepare_warmed_hybrid(context: &LiveContext<'_>) -> BenchmarkResult<()> {
    let scenario = usage_scenarios(context.settings)
        .into_iter()
        .next()
        .ok_or_else(|| io::Error::other("cold Hybrid scenario is missing"))?;
    let cold_payloads = scenario
        .prompts
        .iter()
        .map(|prompt| websocket_payload(&context.settings.model, prompt))
        .collect::<Vec<_>>();
    let cold = context
        .collect_sample(
            BenchmarkPath::Hybrid,
            &PayloadSet {
                http: &[],
                websocket: &cold_payloads,
            },
        )
        .await?;
    validate_cold_hybrid_sample(&cold)?;
    eprintln!(
        "Hybrid 冷启动取证（不参与候选常量）：HTTP={}，WS={}，重试={}",
        cold.http_requests, cold.websocket_messages, cold.retries
    );
    let shared = context.shared_proxy(BenchmarkPath::Hybrid).await?;
    let snapshot = timeout(context.settings.timeout, async {
        loop {
            let snapshot = shared.proxy.connection_snapshot().await;
            if snapshot.prewarm > 0 {
                break snapshot;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .map_err(|_| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            "Hybrid pool did not become ready before the benchmark timeout",
        )
    })?;
    eprintln!(
        "Hybrid 正式采样使用已预热池：current={}，prewarm={}",
        snapshot.current_connections, snapshot.prewarm
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires AI_COVE_API_KEY and the live AI Cove Responses HTTP/WS endpoints"]
async fn live_hybrid_persistent_lifecycle_smoke() -> BenchmarkResult<()> {
    let authorization = env::var("AI_COVE_API_KEY").map_err(benchmark_error)?;
    let settings = BenchmarkSettings::from_env()?;
    let upstream = Url::parse(&settings.upstream).map_err(benchmark_error)?;
    let direct_url = responses_url(&settings.upstream, false)?;
    let context = LiveContext {
        settings: &settings,
        authorization: &authorization,
        upstream: &upstream,
        direct_url: &direct_url,
        http_proxy: OnceCell::new(),
        hybrid_proxy: OnceCell::new(),
    };
    let result = async {
        prepare_warmed_hybrid(&context).await?;
        verify_benchmark_websocket_lifecycle(&context, BenchmarkPath::Hybrid).await
    }
    .await;
    context.stop().await;
    result
}

#[test]
fn rejects_cold_hybrid_evidence_with_retries() {
    let sample = crate::benchmark::Sample {
        e2e: Duration::from_millis(1),
        setup: Duration::ZERO,
        raw_bytes: 1,
        encoded_bytes: 1,
        logical_requests: 1,
        application_messages: 1,
        http_requests: 1,
        websocket_messages: 0,
        response_events: 1,
        websocket_handshakes: 0,
        round_e2e: vec![Duration::from_millis(1)],
        first_events: vec![Duration::from_millis(1)],
        warm_round_e2e: Vec::new(),
        connection_lifetime: None,
        websocket_reconnects: 0,
        messages_per_connection: None,
        retries: 1,
        round_transports: vec![crate::benchmark::RoundTransport::Http],
    };

    let error = validate_cold_hybrid_sample(&sample)
        .expect_err("retried cold evidence must not unlock formal Hybrid sampling")
        .to_string();

    assert!(error.contains("one retry-free HTTP request"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires AI_COVE_API_KEY and the live AI Cove Responses HTTP/WS endpoints"]
async fn live_three_by_four_benchmark() -> BenchmarkResult<()> {
    let authorization = env::var("AI_COVE_API_KEY").map_err(benchmark_error)?;
    let settings = BenchmarkSettings::from_env()?;
    let upstream = Url::parse(&settings.upstream).map_err(benchmark_error)?;
    let direct_url = responses_url(&settings.upstream, false)?;
    let context = LiveContext {
        settings: &settings,
        authorization: &authorization,
        upstream: &upstream,
        direct_url: &direct_url,
        http_proxy: OnceCell::new(),
        hybrid_proxy: OnceCell::new(),
    };
    let result = async {
        let mut disabled = Vec::new();
        let hybrid_preparation = prepare_warmed_hybrid(&context).await;
        if let Err(error) = hybrid_preparation {
            eprintln!("Hybrid cold/warm preparation failed: {error}; 正式采样跳过该路径");
            disable_path(&mut disabled, BenchmarkPath::Hybrid);
        }
        for path in [BenchmarkPath::WebSocket, BenchmarkPath::Hybrid] {
            if disabled.contains(&path) {
                continue;
            }
            if let Err(error) = verify_benchmark_websocket_lifecycle(&context, path).await {
                eprintln!("{error}; 正式采样跳过该路径，其他路径继续");
                disable_path(&mut disabled, path);
            }
        }
        let mut cases = Vec::with_capacity(12);
        for scenario in usage_scenarios(&settings) {
            cases.extend(collect_scenario(&context, scenario, &mut disabled).await?);
        }
        if cases.is_empty() {
            return Err(io::Error::other("benchmark has no usable paths"));
        }
        crate::benchmark::report::print_report(&settings, &cases)?;
        write_calibration_if_requested(&settings, &cases)?;
        Ok(())
    }
    .await;
    context.stop().await;
    result
}
