use std::{env, error::Error, io, sync::Arc};

use url::Url;

use crate::proxy::{Metrics, ProxyOptions, start_proxy};

use super::{
    BenchmarkCase, BenchmarkSettings, http_payload, responses_url, usage_scenarios,
    websocket_payload,
};

mod http;
mod websocket;

fn require_compression(case: &BenchmarkCase) -> Result<(), Box<dyn Error>> {
    if case.scenario == "单轮短上下文" {
        return Ok(());
    }
    if case
        .samples
        .iter()
        .all(|sample| sample.wire_bytes < sample.raw_bytes)
    {
        return Ok(());
    }
    Err(io::Error::other(format!(
        "{} / {} did not produce a smaller wire payload",
        case.scenario, case.path
    ))
    .into())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires AI_COVE_API_KEY and the live AI Cove Responses HTTP/WS endpoints"]
async fn live_three_by_three_benchmark() -> Result<(), Box<dyn Error>> {
    let authorization = env::var("AI_COVE_API_KEY")?;
    let settings = BenchmarkSettings::from_env()?;
    let http_client = reqwest::Client::builder()
        .pool_max_idle_per_host(1)
        .build()?;
    let direct_url = responses_url(&settings.upstream, false)?;

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

    let cases_result: Result<Vec<BenchmarkCase>, Box<dyn Error>> = async {
        let mut cases = Vec::with_capacity(9);
        for scenario in usage_scenarios(&settings) {
            let http_payloads = scenario
                .prompts
                .iter()
                .map(|prompt| http_payload(&settings.model, prompt))
                .collect::<Vec<_>>();
            let websocket_payloads = scenario
                .prompts
                .iter()
                .map(|prompt| websocket_payload(&settings.model, prompt))
                .collect::<Vec<_>>();

            let direct = http::collect_case(
                http::Case {
                    scenario: scenario.name,
                    path: "直连（不走 Turbo）",
                    client: &http_client,
                    url: &direct_url,
                    authorization: &authorization,
                    payloads: &http_payloads,
                    metrics: None,
                },
                &settings,
            )
            .await?;
            let http = http::collect_case(
                http::Case {
                    scenario: scenario.name,
                    path: "Turbo HTTP + zstd",
                    client: &http_client,
                    url: &http_url,
                    authorization: &authorization,
                    payloads: &http_payloads,
                    metrics: Some(http_metrics.as_ref()),
                },
                &settings,
            )
            .await?;
            let websocket = websocket::collect_case(
                websocket::Case {
                    scenario: scenario.name,
                    path: "Turbo WS + zstd",
                    url: &websocket_url,
                    authorization: &authorization,
                    payloads: &websocket_payloads,
                    metrics: websocket_metrics.as_ref(),
                },
                &settings,
            )
            .await?;
            require_compression(&http)?;
            require_compression(&websocket)?;
            cases.extend([direct, http, websocket]);
        }
        Ok(cases)
    }
    .await;

    http_proxy.stop().await;
    websocket_proxy.stop().await;
    let cases = cases_result?;
    super::report::print_report(&settings, &cases)?;
    Ok(())
}
