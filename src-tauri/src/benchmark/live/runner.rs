use std::{env, io, path::PathBuf};

use url::Url;

use super::{
    BenchmarkPath, BenchmarkResult, LiveContext, PayloadSet, benchmark_error, collect_scenario,
    disable_path, websocket::validate_hybrid_lifecycle,
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
        BenchmarkPath::Hybrid if sample.websocket_reconnects == 0 => {
            validate_hybrid_lifecycle(
                sample.http_requests,
                sample.websocket_messages,
                sample.logical_requests,
            )?;
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
    };
    let mut disabled = Vec::new();
    for path in [BenchmarkPath::WebSocket, BenchmarkPath::Hybrid] {
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
