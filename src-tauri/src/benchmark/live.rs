use std::{env, error::Error, io, sync::Arc};

use url::Url;

use crate::proxy::{Metrics, ProxyOptions, start_proxy};

use super::{BenchmarkCase, BenchmarkSettings, http_payload, responses_url, websocket_payload};

mod http;
mod websocket;

fn require_compression(case: &BenchmarkCase) -> Result<(), Box<dyn Error>> {
    if case
        .samples
        .iter()
        .all(|sample| sample.wire_bytes < sample.raw_bytes)
    {
        return Ok(());
    }
    Err(io::Error::other(format!(
        "{} did not produce a smaller wire payload; use a larger repetitive request",
        case.name
    ))
    .into())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires AI_COVE_API_KEY and the live AI Cove Responses HTTP/WS endpoints"]
async fn live_three_path_benchmark() -> Result<(), Box<dyn Error>> {
    let authorization = env::var("AI_COVE_API_KEY")?;
    let settings = BenchmarkSettings::from_env()?;
    let http_client = reqwest::Client::builder()
        .pool_max_idle_per_host(1)
        .build()?;
    let http_payload = http_payload(&settings);
    let websocket_payload = websocket_payload(&settings);
    let direct_url = responses_url(&settings.upstream, false)?;

    let direct = http::collect_case(
        http::Case {
            name: "直连（不走 Turbo）",
            client: &http_client,
            url: &direct_url,
            authorization: &authorization,
            payload: &http_payload,
            metrics: None,
        },
        &settings,
    )
    .await?;

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
    let http_result = http::collect_case(
        http::Case {
            name: "HTTP POST + zstd",
            client: &http_client,
            url: &http_url,
            authorization: &authorization,
            payload: &http_payload,
            metrics: Some(http_metrics.as_ref()),
        },
        &settings,
    )
    .await;
    http_proxy.stop().await;
    let http_compressed = http_result?;
    require_compression(&http_compressed)?;

    let websocket_metrics = Arc::new(Metrics::default());
    let websocket_proxy = start_proxy(ProxyOptions {
        upstream: Url::parse(&settings.upstream)?,
        compression_enabled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        websocket_enabled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        ai_cove_private_websocket_zstd: true,
        metrics: Arc::clone(&websocket_metrics),
        preferred_ports: vec![0],
        max_request_body_bytes: 128 * 1024 * 1024,
    })
    .await?;
    let websocket_url = responses_url(websocket_proxy.endpoint(), true)?;
    let websocket_result = websocket::collect_case(
        &websocket_url,
        &authorization,
        &websocket_payload,
        &settings,
        websocket_metrics.as_ref(),
    )
    .await;
    websocket_proxy.stop().await;
    let websocket_compressed = websocket_result?;
    require_compression(&websocket_compressed)?;

    super::report::print_report(&settings, &[direct, http_compressed, websocket_compressed])?;
    Ok(())
}
