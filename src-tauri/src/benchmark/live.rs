use std::{error::Error, io};

use crate::proxy::Metrics;

use super::{
    BenchmarkCase, BenchmarkSettings, DIRECT_PATH, DIRECT_WS_PATH, HTTP_PATH, Sample,
    UsageScenario, WEBSOCKET_PATH, http_payload, websocket_payload,
};

mod http;
mod runner;
mod websocket;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BenchmarkPath {
    Direct,
    Http,
    DirectWebSocket,
    WebSocket,
}

impl BenchmarkPath {
    const fn label(self) -> &'static str {
        match self {
            Self::Direct => DIRECT_PATH,
            Self::Http => HTTP_PATH,
            Self::DirectWebSocket => DIRECT_WS_PATH,
            Self::WebSocket => WEBSOCKET_PATH,
        }
    }
}

const fn rotated_paths(iteration: usize) -> [BenchmarkPath; 4] {
    match iteration % 4 {
        0 => [
            BenchmarkPath::Direct,
            BenchmarkPath::Http,
            BenchmarkPath::DirectWebSocket,
            BenchmarkPath::WebSocket,
        ],
        1 => [
            BenchmarkPath::Http,
            BenchmarkPath::DirectWebSocket,
            BenchmarkPath::WebSocket,
            BenchmarkPath::Direct,
        ],
        2 => [
            BenchmarkPath::DirectWebSocket,
            BenchmarkPath::WebSocket,
            BenchmarkPath::Direct,
            BenchmarkPath::Http,
        ],
        _ => [
            BenchmarkPath::WebSocket,
            BenchmarkPath::Direct,
            BenchmarkPath::Http,
            BenchmarkPath::DirectWebSocket,
        ],
    }
}

struct PayloadSet<'a> {
    http: &'a [String],
    websocket: &'a [String],
}

struct LiveContext<'a> {
    settings: &'a BenchmarkSettings,
    authorization: &'a str,
    http_client: &'a reqwest::Client,
    direct_url: &'a str,
    http_url: &'a str,
    http_metrics: &'a Metrics,
    direct_websocket_url: &'a str,
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
                http::collect_sample(
                    &http::Case {
                        client: self.http_client,
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
                http::collect_sample(
                    &http::Case {
                        client: self.http_client,
                        url: self.http_url,
                        authorization: self.authorization,
                        payloads: payloads.http,
                        metrics: Some(self.http_metrics),
                    },
                    self.settings,
                )
                .await
            }
            BenchmarkPath::DirectWebSocket => {
                websocket::collect_sample(
                    &websocket::Case {
                        url: self.direct_websocket_url,
                        authorization: self.authorization,
                        payloads: payloads.websocket,
                        metrics: None,
                    },
                    self.settings,
                )
                .await
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
) -> Result<[BenchmarkCase; 4], Box<dyn Error>> {
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
    let mut direct_websocket = BenchmarkCase {
        scenario: scenario.name,
        path: BenchmarkPath::DirectWebSocket.label(),
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
                    BenchmarkPath::DirectWebSocket => direct_websocket.samples.push(sample),
                    BenchmarkPath::WebSocket => websocket.samples.push(sample),
                }
            }
        }
    }
    require_compression(&http, scenario.requires_compression)?;
    require_compression(&websocket, scenario.requires_compression)?;
    Ok([direct, http, direct_websocket, websocket])
}

#[cfg(test)]
mod tests {
    use super::{BenchmarkPath, rotated_paths};

    #[test]
    fn rotates_four_path_order_to_balance_time_drift() {
        assert_eq!(
            rotated_paths(0),
            [
                BenchmarkPath::Direct,
                BenchmarkPath::Http,
                BenchmarkPath::DirectWebSocket,
                BenchmarkPath::WebSocket
            ]
        );
        assert_eq!(
            rotated_paths(1),
            [
                BenchmarkPath::Http,
                BenchmarkPath::DirectWebSocket,
                BenchmarkPath::WebSocket,
                BenchmarkPath::Direct
            ]
        );
        assert_eq!(
            rotated_paths(2),
            [
                BenchmarkPath::DirectWebSocket,
                BenchmarkPath::WebSocket,
                BenchmarkPath::Direct,
                BenchmarkPath::Http
            ]
        );
        assert_eq!(
            rotated_paths(3),
            [
                BenchmarkPath::WebSocket,
                BenchmarkPath::Direct,
                BenchmarkPath::Http,
                BenchmarkPath::DirectWebSocket
            ]
        );
        assert_eq!(rotated_paths(4), rotated_paths(0));
    }
}
