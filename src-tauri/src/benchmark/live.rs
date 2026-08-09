use std::{
    io,
    sync::{Arc, atomic::AtomicBool},
};

use url::Url;

use crate::proxy::{
    Metrics, ProxyOptions, private_websocket_benchmark::start as start_private_websocket_proxy,
    start_proxy,
};

use super::{
    BenchmarkCase, BenchmarkResult, BenchmarkSettings, DIRECT_PATH, HTTP_PATH, HYBRID_PATH, Sample,
    UsageScenario, WEBSOCKET_PATH, benchmark_error, http_payload, websocket_payload,
};

#[cfg(test)]
mod connection_tests;
mod http;
#[cfg(test)]
mod runner;
mod websocket;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BenchmarkPath {
    Direct,
    Http,
    WebSocket,
    Hybrid,
}

impl BenchmarkPath {
    const fn label(self) -> &'static str {
        match self {
            Self::Direct => DIRECT_PATH,
            Self::Http => HTTP_PATH,
            Self::WebSocket => WEBSOCKET_PATH,
            Self::Hybrid => HYBRID_PATH,
        }
    }
}

const fn rotated_paths(iteration: usize) -> [BenchmarkPath; 4] {
    match iteration % 4 {
        0 => [
            BenchmarkPath::Direct,
            BenchmarkPath::Http,
            BenchmarkPath::WebSocket,
            BenchmarkPath::Hybrid,
        ],
        1 => [
            BenchmarkPath::Http,
            BenchmarkPath::WebSocket,
            BenchmarkPath::Hybrid,
            BenchmarkPath::Direct,
        ],
        2 => [
            BenchmarkPath::WebSocket,
            BenchmarkPath::Hybrid,
            BenchmarkPath::Direct,
            BenchmarkPath::Http,
        ],
        _ => [
            BenchmarkPath::Hybrid,
            BenchmarkPath::Direct,
            BenchmarkPath::Http,
            BenchmarkPath::WebSocket,
        ],
    }
}

fn fresh_http_client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder().pool_max_idle_per_host(1).build()
}

struct PayloadSet<'a> {
    http: &'a [String],
    websocket: &'a [String],
}

struct LiveContext<'a> {
    settings: &'a BenchmarkSettings,
    authorization: &'a str,
    upstream: &'a Url,
    direct_url: &'a str,
}

impl LiveContext<'_> {
    async fn collect_websocket_sample(
        &self,
        mode: websocket::Mode,
        payloads: &[String],
    ) -> BenchmarkResult<Sample> {
        let metrics = Arc::new(Metrics::default());
        let proxy = match mode {
            websocket::Mode::PrivateWebSocket => {
                start_private_websocket_proxy(self.upstream.clone(), Arc::clone(&metrics))
                    .await
                    .map_err(benchmark_error)?
            }
            websocket::Mode::Hybrid => start_proxy(ProxyOptions {
                upstream: self.upstream.clone(),
                compression_enabled: Arc::new(AtomicBool::new(true)),
                websocket_enabled: Arc::new(AtomicBool::new(true)),
                ai_cove_private_websocket_zstd: true,
                metrics: Arc::clone(&metrics),
                preferred_ports: vec![0],
                max_request_body_bytes: 128 * 1024 * 1024,
            })
            .await
            .map_err(benchmark_error)?,
        };
        let result = async {
            let url = super::responses_url(proxy.endpoint(), true)?;
            websocket::collect_sample(
                &websocket::Case {
                    url: &url,
                    authorization: self.authorization,
                    payloads,
                    metrics: Some(metrics.as_ref()),
                    mode,
                },
                self.settings,
            )
            .await
        }
        .await;
        proxy.stop().await;
        result
    }

    async fn collect_sample(
        &self,
        path: BenchmarkPath,
        payloads: &PayloadSet<'_>,
    ) -> BenchmarkResult<Sample> {
        match self.collect_sample_once(path, payloads).await {
            Err(error) if error.kind() == io::ErrorKind::ConnectionAborted => {
                eprintln!(
                    "基准路径 {} 遇到临时上游错误，将完整重试一次：{error}",
                    path.label()
                );
                let mut sample = self.collect_sample_once(path, payloads).await?;
                sample.retries = 1;
                Ok(sample)
            }
            result => result,
        }
    }

    async fn collect_sample_once(
        &self,
        path: BenchmarkPath,
        payloads: &PayloadSet<'_>,
    ) -> BenchmarkResult<Sample> {
        match path {
            BenchmarkPath::Direct => {
                let client = fresh_http_client().map_err(benchmark_error)?;
                http::collect_sample(
                    &http::Case {
                        client: &client,
                        url: self.direct_url,
                        authorization: self.authorization,
                        payloads: payloads.http,
                        metrics: None,
                    },
                    self.settings,
                )
                .await
            }
            BenchmarkPath::Http => {
                let metrics = Arc::new(Metrics::default());
                let proxy = start_proxy(ProxyOptions {
                    upstream: self.upstream.clone(),
                    compression_enabled: Arc::new(AtomicBool::new(true)),
                    websocket_enabled: Arc::new(AtomicBool::new(false)),
                    ai_cove_private_websocket_zstd: false,
                    metrics: Arc::clone(&metrics),
                    preferred_ports: vec![0],
                    max_request_body_bytes: 128 * 1024 * 1024,
                })
                .await
                .map_err(benchmark_error)?;
                let result = async {
                    let client = fresh_http_client().map_err(benchmark_error)?;
                    let url = super::responses_url(proxy.endpoint(), false)?;
                    http::collect_sample(
                        &http::Case {
                            client: &client,
                            url: &url,
                            authorization: self.authorization,
                            payloads: payloads.http,
                            metrics: Some(metrics.as_ref()),
                        },
                        self.settings,
                    )
                    .await
                }
                .await;
                proxy.stop().await;
                result
            }
            BenchmarkPath::WebSocket => {
                self.collect_websocket_sample(websocket::Mode::PrivateWebSocket, payloads.websocket)
                    .await
            }
            BenchmarkPath::Hybrid => {
                self.collect_websocket_sample(websocket::Mode::Hybrid, payloads.websocket)
                    .await
            }
        }
    }
}

fn require_compression(case: &BenchmarkCase, required: bool) -> BenchmarkResult<()> {
    if !required {
        return Ok(());
    }
    let mut samples = case
        .samples
        .iter()
        .filter(|sample| sample.retries == 0)
        .peekable();
    if samples.peek().is_none() {
        return Err(io::Error::other(format!(
            "benchmark scenario={} path={} 没有无重试的有效样本",
            case.scenario, case.path
        )));
    }
    if samples.all(|sample| sample.encoded_bytes < sample.raw_bytes) {
        return Ok(());
    }
    Err(io::Error::other(format!(
        "{} / {} did not produce a smaller encoded payload",
        case.scenario, case.path
    )))
}

fn sample_context_error(
    scenario: &str,
    path: BenchmarkPath,
    iteration: usize,
    error: &io::Error,
) -> io::Error {
    io::Error::other(format!(
        "benchmark scenario={scenario} path={} iteration={iteration} failed: {error}",
        path.label(),
    ))
}

async fn collect_scenario(
    context: &LiveContext<'_>,
    scenario: UsageScenario,
) -> BenchmarkResult<[BenchmarkCase; 4]> {
    let http_payloads = scenario
        .prompts
        .iter()
        .map(|prompt| http_payload(&context.settings.model, prompt))
        .collect::<Vec<_>>();
    let websocket_payloads = scenario
        .prompts
        .iter()
        .map(|prompt| websocket_payload(&context.settings.model, prompt))
        .collect::<Vec<_>>();
    let payloads = PayloadSet {
        http: &http_payloads,
        websocket: &websocket_payloads,
    };
    let mut direct = BenchmarkCase {
        scenario: scenario.name,
        path: BenchmarkPath::Direct.label(),
        samples: Vec::with_capacity(context.settings.runs),
    };
    let mut http = BenchmarkCase {
        scenario: scenario.name,
        path: BenchmarkPath::Http.label(),
        samples: Vec::with_capacity(context.settings.runs),
    };
    let mut websocket = BenchmarkCase {
        scenario: scenario.name,
        path: BenchmarkPath::WebSocket.label(),
        samples: Vec::with_capacity(context.settings.runs),
    };
    let mut hybrid = BenchmarkCase {
        scenario: scenario.name,
        path: BenchmarkPath::Hybrid.label(),
        samples: Vec::with_capacity(context.settings.runs),
    };
    let iterations = context
        .settings
        .warmups
        .checked_add(context.settings.runs)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "too many benchmark runs"))?;
    for iteration in 0..iterations {
        for path in rotated_paths(iteration) {
            let sample = context
                .collect_sample(path, &payloads)
                .await
                .map_err(|error| {
                    sample_context_error(scenario.name, path, iteration + 1, &error)
                })?;
            if iteration >= context.settings.warmups {
                match path {
                    BenchmarkPath::Direct => direct.samples.push(sample),
                    BenchmarkPath::Http => http.samples.push(sample),
                    BenchmarkPath::WebSocket => websocket.samples.push(sample),
                    BenchmarkPath::Hybrid => hybrid.samples.push(sample),
                }
            }
        }
    }
    require_compression(&http, scenario.requires_compression)?;
    require_compression(&websocket, scenario.requires_compression)?;
    require_compression(&hybrid, scenario.requires_compression)?;
    Ok([direct, http, websocket, hybrid])
}
