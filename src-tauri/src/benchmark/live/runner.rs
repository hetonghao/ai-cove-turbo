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
        .collect_sample(BenchmarkPath::DirectWebSocket, &payloads)
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
async fn live_three_by_four_benchmark() -> Result<(), Box<dyn Error>> {
    let authorization = env::var("AI_COVE_API_KEY")?;
    let settings = BenchmarkSettings::from_env()?;
    let http_client = reqwest::Client::builder()
        .pool_max_idle_per_host(1)
        .build()?;
    let direct_url = responses_url(&settings.upstream, false)?;
    let direct_websocket_url = responses_url(&settings.upstream, true)?;
    let http_metrics = Arc::new(Metrics::default());
    let http_proxy = start_proxy(ProxyOptions {
        upstream: Url::parse(&settings.upstream)?,
        compression_enabled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        websocket_enabled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        ai_cove_private_websocket_zstd: false,
        metrics: Arc::clone(&http_metrics),
        preferred_ports: vec![0],
        max_request_body_bytes: 128 * 1024 * 1024,
    })
    .await?;
    let http_url = responses_url(http_proxy.endpoint(), false)?;
    let websocket_metrics = Arc::new(Metrics::default());
    let websocket_proxy = match start_proxy(ProxyOptions {
        upstream: Url::parse(&settings.upstream)?,
        compression_enabled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        websocket_enabled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        ai_cove_private_websocket_zstd: true,
        metrics: Arc::clone(&websocket_metrics),
        preferred_ports: vec![0],
        max_request_body_bytes: 128 * 1024 * 1024,
    })
    .await
    {
        Ok(proxy) => proxy,
        Err(error) => {
            http_proxy.stop().await;
            return Err(error.into());
        }
    };
    let websocket_url = responses_url(websocket_proxy.endpoint(), true)?;
    let context = LiveContext {
        settings: &settings,
        authorization: &authorization,
        http_client: &http_client,
        direct_url: &direct_url,
        http_url: &http_url,
        http_metrics: http_metrics.as_ref(),
        direct_websocket_url: &direct_websocket_url,
        websocket_url: &websocket_url,
        websocket_metrics: websocket_metrics.as_ref(),
    };
    let cases_result: Result<Vec<BenchmarkCase>, Box<dyn Error>> = async {
        verify_benchmark_websocket_lifecycle(&context).await?;
        let mut cases = Vec::with_capacity(12);
        for scenario in usage_scenarios(&settings) {
            cases.extend(collect_scenario(&context, scenario).await?);
        }
        Ok(cases)
    }
    .await;
    http_proxy.stop().await;
    websocket_proxy.stop().await;
    let cases = cases_result?;
    crate::benchmark::report::print_report(&settings, &cases)?;
    Ok(())
}
