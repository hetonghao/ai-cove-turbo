use std::{
    io,
    sync::{Arc, atomic::AtomicBool},
};

use url::Url;

use crate::proxy::{
    Metrics, ProxyHandle, ProxyOptions,
    private_websocket_benchmark::start as start_private_websocket_proxy, start_proxy,
};
use tokio::sync::OnceCell;

use super::{
    BenchmarkResult, BenchmarkSettings, DIRECT_PATH, HTTP_PATH, HYBRID_PATH, Sample,
    WEBSOCKET_PATH, benchmark_error,
};

mod collection;
#[cfg(test)]
mod connection_tests;
mod http;
#[cfg(test)]
mod runner;
mod websocket;

use collection::{collect_scenario, disable_path, require_compression, sample_context_error};

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
    http_proxy: OnceCell<SharedProxy>,
    hybrid_proxy: OnceCell<SharedProxy>,
}

struct SharedProxy {
    metrics: Arc<Metrics>,
    proxy: ProxyHandle,
}

impl LiveContext<'_> {
    async fn shared_proxy(&self, path: BenchmarkPath) -> BenchmarkResult<&SharedProxy> {
        let cell = match path {
            BenchmarkPath::Http => &self.http_proxy,
            BenchmarkPath::Hybrid => &self.hybrid_proxy,
            BenchmarkPath::Direct | BenchmarkPath::WebSocket => {
                return Err(io::Error::other(format!(
                    "{} does not use a shared Turbo proxy",
                    path.label()
                )));
            }
        };
        cell.get_or_try_init(|| async move {
            let metrics = Arc::new(Metrics::default());
            let websocket_enabled = path == BenchmarkPath::Hybrid;
            let proxy = start_proxy(ProxyOptions {
                upstream: self.upstream.clone(),
                compression_enabled: Arc::new(AtomicBool::new(true)),
                websocket_enabled: Arc::new(AtomicBool::new(websocket_enabled)),
                ai_cove_private_websocket_zstd: websocket_enabled,
                metrics: Arc::clone(&metrics),
                preferred_ports: vec![0],
                max_request_body_bytes: 128 * 1024 * 1024,
            })
            .await
            .map_err(benchmark_error)?;
            Ok(SharedProxy { metrics, proxy })
        })
        .await
    }

    async fn stop(self) {
        if let Some(shared) = self.http_proxy.into_inner() {
            shared.proxy.stop().await;
        }
        if let Some(shared) = self.hybrid_proxy.into_inner() {
            shared.proxy.stop().await;
        }
    }

    async fn collect_websocket_sample(
        &self,
        mode: websocket::Mode,
        payloads: &[String],
    ) -> BenchmarkResult<Sample> {
        match mode {
            websocket::Mode::PrivateWebSocket => {
                let metrics = Arc::new(Metrics::default());
                let proxy =
                    start_private_websocket_proxy(self.upstream.clone(), Arc::clone(&metrics))
                        .await
                        .map_err(benchmark_error)?;
                let result = websocket::collect_sample(
                    &websocket::Case {
                        url: &super::responses_url(proxy.endpoint(), true)?,
                        authorization: self.authorization,
                        payloads,
                        metrics: Some(metrics.as_ref()),
                        mode,
                    },
                    self.settings,
                )
                .await;
                proxy.stop().await;
                result
            }
            websocket::Mode::Hybrid => {
                let shared = self.shared_proxy(BenchmarkPath::Hybrid).await?;
                websocket::collect_sample(
                    &websocket::Case {
                        url: &super::responses_url(shared.proxy.endpoint(), true)?,
                        authorization: self.authorization,
                        payloads,
                        metrics: Some(shared.metrics.as_ref()),
                        mode,
                    },
                    self.settings,
                )
                .await
            }
        }
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
                let shared = self.shared_proxy(BenchmarkPath::Http).await?;
                let client = fresh_http_client().map_err(benchmark_error)?;
                http::collect_sample(
                    &http::Case {
                        client: &client,
                        url: &super::responses_url(shared.proxy.endpoint(), false)?,
                        authorization: self.authorization,
                        payloads: payloads.http,
                        metrics: Some(shared.metrics.as_ref()),
                    },
                    self.settings,
                )
                .await
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
