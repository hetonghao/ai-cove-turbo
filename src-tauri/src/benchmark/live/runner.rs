use std::{env, error::Error, io, sync::Arc};

use url::Url;

use crate::proxy::{Metrics, ProxyOptions, start_proxy};

use super::{BenchmarkPath, LiveContext, PayloadSet, collect_scenario};
use crate::benchmark::{
    BenchmarkCase, BenchmarkSettings, DEFAULT_MULTI_ROUNDS, responses_url, usage_scenarios,
    websocket_payload,
};

pub(super) async fn verify_benchmark_websocket_lifecycle(
    context: &LiveContext<'_>,
) -> Result<(), Box<dyn Error>> {
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
        .collect_sample(BenchmarkPath::WebSocket, &payloads)
        .await?;
    let expected_messages = u64::try_from(websocket_payloads.len())?;
    if sample.websocket_handshakes != 1
        || sample.application_messages != expected_messages
        || sample.messages_per_connection != Some(expected_messages)
        || sample.websocket_reconnects != 0
    {
        return Err(io::Error::other(format!(
            "benchmark-client WebSocket lifecycle gate failed: handshakes={}, application_messages={}, messages_per_connection={:?}, reconnects={}",
            sample.websocket_handshakes,
            sample.application_messages,
            sample.messages_per_connection,
            sample.websocket_reconnects,
        ))
        .into());
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires AI_COVE_API_KEY and the live AI Cove Responses HTTP/WS endpoints"]
async fn live_three_by_three_benchmark() -> Result<(), Box<dyn Error>> {
    let authorization = env::var("AI_COVE_API_KEY")?;
    let settings = BenchmarkSettings::from_env()?;
    let upstream = Url::parse(&settings.upstream)?;
    let direct_url = responses_url(&settings.upstream, false)?;
    let websocket_metrics = Arc::new(Metrics::default());
    let websocket_proxy = start_proxy(ProxyOptions {
        upstream: upstream.clone(),
        compression_enabled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        websocket_enabled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        ai_cove_private_websocket_zstd: true,
        metrics: Arc::clone(&websocket_metrics),
        preferred_ports: vec![0],
        max_request_body_bytes: 128 * 1024 * 1024,
    })
    .await?;
    let websocket_url = responses_url(websocket_proxy.endpoint(), true)?;
    let context = LiveContext {
        settings: &settings,
        authorization: &authorization,
        upstream: &upstream,
        direct_url: &direct_url,
        websocket_url: &websocket_url,
        websocket_metrics: websocket_metrics.as_ref(),
    };
    let cases_result: Result<Vec<BenchmarkCase>, Box<dyn Error>> = async {
        verify_benchmark_websocket_lifecycle(&context).await?;
        let mut cases = Vec::with_capacity(9);
        for scenario in usage_scenarios(&settings) {
            cases.extend(collect_scenario(&context, scenario).await?);
        }
        Ok(cases)
    }
    .await;
    websocket_proxy.stop().await;
    let cases = cases_result?;
    crate::benchmark::report::print_report(&settings, &cases)?;
    Ok(())
}
