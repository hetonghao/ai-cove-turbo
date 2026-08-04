use std::{
    error::Error,
    io,
    sync::{Arc, atomic::AtomicBool},
};

use url::Url;

use crate::proxy::{Metrics, ProxyOptions, start_proxy};

use super::{
    BenchmarkCase, BenchmarkSettings, DIRECT_PATH, HTTP_PATH, Sample, UsageScenario,
    WEBSOCKET_PATH, http_payload, websocket_payload,
};

#[cfg(test)]
mod connection_tests;
mod http;
mod runner;
mod websocket;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BenchmarkPath {
    Direct,
    Http,
    WebSocket,
}

impl BenchmarkPath {
    const fn label(self) -> &'static str {
        match self {
            Self::Direct => DIRECT_PATH,
            Self::Http => HTTP_PATH,
            Self::WebSocket => WEBSOCKET_PATH,
        }
    }
}

const fn rotated_paths(iteration: usize) -> [BenchmarkPath; 3] {
    match iteration % 3 {
        0 => [
            BenchmarkPath::Direct,
            BenchmarkPath::Http,
            BenchmarkPath::WebSocket,
        ],
        1 => [
            BenchmarkPath::Http,
            BenchmarkPath::WebSocket,
            BenchmarkPath::Direct,
        ],
        _ => [
            BenchmarkPath::WebSocket,
            BenchmarkPath::Direct,
            BenchmarkPath::Http,
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
    websocket_url: &'a str,
    websocket_metrics: &'a Metrics,
}

impl LiveContext<'_> {
    async fn collect_sample(
        &self,
        path: BenchmarkPath,
        payloads: &PayloadSet<'_>,
    ) -> Result<Sample, Box<dyn Error>> {
        match path {
            BenchmarkPath::Direct => {
                let client = fresh_http_client()?;
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
                .await?;
                let result = async {
                    let client = fresh_http_client()?;
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
                let result = result.map_err(|error| io::Error::other(error.to_string()));
                proxy.stop().await;
                result.map_err(Into::into)
            }
            BenchmarkPath::WebSocket => {
                websocket::collect_sample(
                    &websocket::Case {
                        url: self.websocket_url,
                        authorization: self.authorization,
                        payloads: payloads.websocket,
                        metrics: Some(self.websocket_metrics),
                    },
                    self.settings,
                )
                .await
            }
        }
    }
}

fn require_compression(case: &BenchmarkCase, required: bool) -> Result<(), Box<dyn Error>> {
    if !required
        || case
            .samples
            .iter()
            .all(|sample| sample.encoded_bytes < sample.raw_bytes)
    {
        return Ok(());
    }
    Err(io::Error::other(format!(
        "{} / {} did not produce a smaller encoded payload",
        case.scenario, case.path
    ))
    .into())
}

async fn collect_scenario(
    context: &LiveContext<'_>,
    scenario: UsageScenario,
) -> Result<[BenchmarkCase; 3], Box<dyn Error>> {
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
    let iterations = context
        .settings
        .warmups
        .checked_add(context.settings.runs)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "too many benchmark runs"))?;
    for iteration in 0..iterations {
        for path in rotated_paths(iteration) {
            let sample = context.collect_sample(path, &payloads).await?;
            if iteration >= context.settings.warmups {
                match path {
                    BenchmarkPath::Direct => direct.samples.push(sample),
                    BenchmarkPath::Http => http.samples.push(sample),
                    BenchmarkPath::WebSocket => websocket.samples.push(sample),
                }
            }
        }
    }
    require_compression(&http, scenario.requires_compression)?;
    require_compression(&websocket, scenario.requires_compression)?;
    Ok([direct, http, websocket])
}

#[cfg(test)]
mod tests {
    use super::{BenchmarkPath, rotated_paths};

    #[test]
    fn rotates_three_path_order_to_balance_time_drift() {
        assert_eq!(
            rotated_paths(0),
            [
                BenchmarkPath::Direct,
                BenchmarkPath::Http,
                BenchmarkPath::WebSocket
            ]
        );
        assert_eq!(
            rotated_paths(1),
            [
                BenchmarkPath::Http,
                BenchmarkPath::WebSocket,
                BenchmarkPath::Direct
            ]
        );
        assert_eq!(
            rotated_paths(2),
            [
                BenchmarkPath::WebSocket,
                BenchmarkPath::Direct,
                BenchmarkPath::Http
            ]
        );
        assert_eq!(rotated_paths(3), rotated_paths(0));
    }
}
