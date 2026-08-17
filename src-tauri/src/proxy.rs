use std::{
    collections::HashSet,
    fmt,
    io::Cursor,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use axum::{
    Router,
    body::{Body, Bytes, to_bytes},
    extract::{Request as AxumRequest, State},
    http::{HeaderMap, HeaderName, Method, Response, StatusCode, Uri, Version, header},
    response::IntoResponse,
    routing::get,
};
use futures_util::TryStreamExt;
use http_body_util::Empty;
use hyper_rustls::{ConfigBuilderExt, HttpsConnector, HttpsConnectorBuilder};
use hyper_util::{
    client::legacy::{Client, connect::HttpConnector},
    rt::{TokioExecutor, TokioIo},
};
use serde::Serialize;
use tokio::{io::copy_bidirectional, net::TcpListener, sync::oneshot, task::JoinHandle};
use url::Url;

mod hybrid;
mod hybrid_pool;
mod private_websocket;
#[cfg(test)]
#[path = "proxy/private_websocket_benchmark.rs"]
pub(crate) mod private_websocket_benchmark;
pub(crate) mod traffic;

pub(crate) use hybrid_pool::ConnectionSnapshot;
use private_websocket::{PrivateTlsConfig, client_upgrade_response};

#[cfg(test)]
use private_websocket::encode_private_message_async;

#[cfg(test)]
use private_websocket::{
    PRIVATE_ENVELOPE_HEADER_BYTES, PRIVATE_WEBSOCKET_SUBPROTOCOL, decode_private_message,
    encode_private_message,
};

const DEFAULT_MAX_REQUEST_BODY_BYTES: usize = 128 * 1024 * 1024;
pub(crate) const MIN_COMPRESSION_INPUT_BYTES: usize = 1024;
const PRIVATE_TLS_SESSION_CACHE_SIZE: usize = 256;
const fn turbo_client_version() -> &'static str {
    if cfg!(target_os = "windows") {
        concat!("win/", env!("CARGO_PKG_VERSION"))
    } else if cfg!(target_os = "macos") {
        concat!("mac/", env!("CARGO_PKG_VERSION"))
    } else {
        concat!("other/", env!("CARGO_PKG_VERSION"))
    }
}
const HOP_BY_HOP_HEADERS: [&str; 8] = [
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

#[derive(Debug)]
pub(crate) struct ProxyOptions {
    pub(crate) upstream: Url,
    pub(crate) compression_enabled: Arc<AtomicBool>,
    pub(crate) websocket_enabled: Arc<AtomicBool>,
    pub(crate) ai_cove_private_websocket_zstd: bool,
    pub(crate) metrics: Arc<Metrics>,
    pub(crate) preferred_ports: Vec<u16>,
    pub(crate) max_request_body_bytes: usize,
}

#[derive(Debug, Default)]
pub(crate) struct Metrics {
    requests: AtomicU64,
    successful_responses: AtomicU64,
    raw_bytes: AtomicU64,
    sent_bytes: AtomicU64,
    compression_verified: AtomicBool,
    websocket_verified: AtomicBool,
    websocket_zstd_verified: AtomicBool,
    websocket_handshakes: AtomicU64,
    websocket_active: AtomicU64,
    websocket_failures: AtomicU64,
    websocket_messages: AtomicU64,
    websocket_raw_bytes: AtomicU64,
    websocket_sent_bytes: AtomicU64,
    http_fallbacks: AtomicU64,
    traffic: traffic::TrafficStore,
    #[cfg(test)]
    traffic_recorded: tokio::sync::Notify,
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MetricsSnapshot {
    pub(crate) requests: u64,
    pub(crate) successful_responses: u64,
    pub(crate) raw_bytes: u64,
    pub(crate) sent_bytes: u64,
    pub(crate) compression_verified: bool,
    pub(crate) websocket_verified: bool,
    pub(crate) websocket_zstd_verified: bool,
    pub(crate) websocket_handshakes: u64,
    pub(crate) websocket_active: u64,
    pub(crate) websocket_failures: u64,
    pub(crate) websocket_messages: u64,
    pub(crate) websocket_raw_bytes: u64,
    pub(crate) websocket_sent_bytes: u64,
    pub(crate) http_fallbacks: u64,
    pub(crate) hybrid_ws: u64,
    pub(crate) hybrid_cold_start_http: u64,
    pub(crate) hybrid_recovery_http: u64,
    pub(crate) hybrid_large_request_http: u64,
    pub(crate) direct_http: u64,
}

#[derive(Clone, Copy)]
struct HttpRequestMetric<'a> {
    path: &'a str,
    status: u16,
    raw_bytes: usize,
    sent_bytes: usize,
    compressed: bool,
    result: traffic::TrafficResult,
    route: traffic::TrafficRoute,
    failure_reason: Option<&'a str>,
}

#[derive(Clone, Copy)]
struct HttpTraffic {
    result: traffic::TrafficResult,
    route: traffic::TrafficRoute,
}

impl HttpTraffic {
    const DIRECT: Self = Self {
        result: traffic::TrafficResult::Success,
        route: traffic::TrafficRoute::DirectHttp,
    };
    const HYBRID_COLD_START: Self = Self {
        result: traffic::TrafficResult::Success,
        route: traffic::TrafficRoute::HybridColdStartHttp,
    };
    const HYBRID_RECOVERY: Self = Self {
        result: traffic::TrafficResult::Fallback,
        route: traffic::TrafficRoute::HybridRecoveryHttp,
    };
    const HYBRID_LARGE_REQUEST: Self = Self {
        result: traffic::TrafficResult::Success,
        route: traffic::TrafficRoute::HybridLargeRequestHttp,
    };
}

const fn is_context_length_exceeded(code: u16) -> bool {
    matches!(code, 413 | 1009)
}

impl Metrics {
    pub(crate) fn load_traffic(path: &Path) -> Self {
        Self {
            traffic: traffic::TrafficStore::load(path),
            ..Self::default()
        }
    }

    pub(crate) fn save_traffic(&self, path: &Path) -> std::io::Result<()> {
        self.traffic.save(path)
    }

    pub(crate) fn compact_traffic(&self, path: &Path) -> std::io::Result<()> {
        self.traffic.compact(path)
    }

    pub(crate) fn snapshot(&self) -> MetricsSnapshot {
        let route_counts = self.traffic.route_counts();
        MetricsSnapshot {
            requests: self.requests.load(Ordering::Relaxed),
            successful_responses: self.successful_responses.load(Ordering::Relaxed),
            raw_bytes: self.raw_bytes.load(Ordering::Relaxed),
            sent_bytes: self.sent_bytes.load(Ordering::Relaxed),
            compression_verified: self.compression_verified.load(Ordering::Relaxed),
            websocket_verified: self.websocket_verified.load(Ordering::Relaxed),
            websocket_zstd_verified: self.websocket_zstd_verified.load(Ordering::Relaxed),
            websocket_handshakes: self.websocket_handshakes.load(Ordering::Relaxed),
            websocket_active: self.websocket_active.load(Ordering::Relaxed),
            websocket_failures: self.websocket_failures.load(Ordering::Relaxed),
            websocket_messages: self.websocket_messages.load(Ordering::Relaxed),
            websocket_raw_bytes: self.websocket_raw_bytes.load(Ordering::Relaxed),
            websocket_sent_bytes: self.websocket_sent_bytes.load(Ordering::Relaxed),
            http_fallbacks: self.http_fallbacks.load(Ordering::Relaxed),
            hybrid_ws: route_counts.hybrid_ws,
            hybrid_cold_start_http: route_counts.hybrid_cold_start_http,
            hybrid_recovery_http: route_counts.hybrid_recovery_http,
            hybrid_large_request_http: route_counts.hybrid_large_request_http,
            direct_http: route_counts.direct_http,
        }
    }

    fn record_http(&self, record: HttpRequestMetric<'_>) {
        let result = if record.status >= 400 {
            traffic::TrafficResult::Error
        } else {
            record.result
        };
        let hybrid_capacity_failure = record.route != traffic::TrafficRoute::DirectHttp
            && is_context_length_exceeded(record.status);
        self.requests.fetch_add(1, Ordering::Relaxed);
        if record.path == "/v1/responses"
            && (200..300).contains(&record.status)
            && result != traffic::TrafficResult::Error
        {
            self.successful_responses.fetch_add(1, Ordering::Relaxed);
        }
        self.raw_bytes
            .fetch_add(record.raw_bytes as u64, Ordering::Relaxed);
        self.sent_bytes
            .fetch_add(record.sent_bytes as u64, Ordering::Relaxed);
        if record.compressed {
            self.compression_verified.store(true, Ordering::Relaxed);
        }
        if result == traffic::TrafficResult::Fallback {
            self.http_fallbacks.fetch_add(1, Ordering::Relaxed);
        }
        self.traffic.record(traffic::TrafficRecord {
            timestamp_ms: traffic::now_ms(),
            status: record.status,
            path: record.path,
            raw_bytes: record.raw_bytes as u64,
            sent_bytes: record.sent_bytes as u64,
            transport: traffic::TrafficTransport::Http,
            result,
            route: Some(record.route),
            failure_phase: hybrid_capacity_failure.then_some(traffic::FailurePhase::HybridActive),
            failure_reason: record.failure_reason,
        });
    }

    pub(crate) fn reset_compression_verification(&self) {
        self.compression_verified.store(false, Ordering::Relaxed);
    }

    pub(crate) fn reset_websocket_verification(&self) {
        self.websocket_verified.store(false, Ordering::Relaxed);
        self.websocket_zstd_verified.store(false, Ordering::Relaxed);
        self.websocket_failures.store(0, Ordering::Relaxed);
    }

    fn record_websocket_connected(&self) {
        self.websocket_verified.store(true, Ordering::Relaxed);
        self.websocket_handshakes.fetch_add(1, Ordering::Relaxed);
        self.websocket_active.fetch_add(1, Ordering::Relaxed);
    }

    fn record_websocket_closed(&self) {
        let _ =
            self.websocket_active
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |active| {
                    active.checked_sub(1)
                });
    }

    fn record_websocket_failure(&self) {
        self.websocket_failures.fetch_add(1, Ordering::Relaxed);
    }

    fn record_websocket_error(&self, path: &str, status: u16) {
        self.record_websocket_failure();
        self.record_websocket_traffic(traffic::TrafficRecord {
            timestamp_ms: traffic::now_ms(),
            status,
            path,
            raw_bytes: 0,
            sent_bytes: 0,
            transport: traffic::TrafficTransport::Ws,
            result: traffic::TrafficResult::Error,
            route: None,
            failure_phase: None,
            failure_reason: None,
        });
    }

    fn record_websocket_diagnostic(
        &self,
        path: &str,
        status: u16,
        phase: traffic::FailurePhase,
        reason: &str,
    ) {
        self.record_websocket_failure();
        self.record_websocket_traffic(traffic::TrafficRecord {
            timestamp_ms: traffic::now_ms(),
            status,
            path,
            raw_bytes: 0,
            sent_bytes: 0,
            transport: traffic::TrafficTransport::Ws,
            result: traffic::TrafficResult::Error,
            route: Some(traffic::TrafficRoute::HybridWs),
            failure_phase: Some(phase),
            failure_reason: Some(reason),
        });
    }

    fn record_websocket_zstd_message(
        &self,
        path: &str,
        raw_bytes: usize,
        sent_bytes: usize,
        compressed: bool,
        route: Option<traffic::TrafficRoute>,
    ) {
        self.record_websocket_message(
            traffic::TrafficRecord {
                timestamp_ms: traffic::now_ms(),
                status: StatusCode::SWITCHING_PROTOCOLS.as_u16(),
                path,
                raw_bytes: raw_bytes as u64,
                sent_bytes: sent_bytes as u64,
                transport: traffic::TrafficTransport::Ws,
                result: traffic::TrafficResult::Success,
                route,
                failure_phase: None,
                failure_reason: None,
            },
            compressed,
        );
    }

    fn record_websocket_outcome(&self, record: traffic::TrafficRecord<'_>, compressed: bool) {
        if record.path == "/v1/responses" && record.result == traffic::TrafficResult::Success {
            self.successful_responses.fetch_add(1, Ordering::Relaxed);
        }
        self.record_websocket_message(record, compressed);
    }

    fn record_websocket_message(&self, record: traffic::TrafficRecord<'_>, compressed: bool) {
        self.websocket_messages.fetch_add(1, Ordering::Relaxed);
        self.websocket_raw_bytes
            .fetch_add(record.raw_bytes, Ordering::Relaxed);
        self.websocket_sent_bytes
            .fetch_add(record.sent_bytes, Ordering::Relaxed);
        if compressed {
            self.websocket_zstd_verified.store(true, Ordering::Relaxed);
        }
        if record.result == traffic::TrafficResult::Error {
            self.record_websocket_failure();
        }
        self.record_websocket_traffic(record);
    }

    fn record_websocket_traffic(&self, record: traffic::TrafficRecord<'_>) {
        self.traffic.record(record);
        #[cfg(test)]
        self.traffic_recorded.notify_one();
    }

    pub(crate) fn traffic_snapshot(&self) -> traffic::TrafficSnapshot {
        self.traffic.snapshot()
    }

    #[cfg(test)]
    pub(crate) fn record_test_traffic(&self) {
        self.traffic.record(traffic::TrafficRecord {
            timestamp_ms: traffic::now_ms(),
            status: 200,
            path: "/test",
            raw_bytes: 10,
            sent_bytes: 5,
            transport: traffic::TrafficTransport::Http,
            result: traffic::TrafficResult::Success,
            route: None,
            failure_phase: None,
            failure_reason: None,
        });
    }

    #[cfg(test)]
    pub(crate) fn record_successful_response_for_test(&self) {
        self.record_http(HttpRequestMetric {
            path: "/v1/responses",
            status: StatusCode::OK.as_u16(),
            raw_bytes: 10,
            sent_bytes: 10,
            compressed: false,
            result: traffic::TrafficResult::Success,
            route: traffic::TrafficRoute::DirectHttp,
            failure_reason: None,
        });
    }

    #[cfg(test)]
    pub(crate) fn record_failed_response_for_test(&self) {
        self.record_http(HttpRequestMetric {
            path: "/v1/responses",
            status: StatusCode::BAD_GATEWAY.as_u16(),
            raw_bytes: 10,
            sent_bytes: 10,
            compressed: false,
            result: traffic::TrafficResult::Success,
            route: traffic::TrafficRoute::DirectHttp,
            failure_reason: None,
        });
    }
}

#[derive(Debug)]
pub(crate) struct ProxyHandle {
    endpoint: String,
    hybrid_pool: hybrid_pool::HybridPool,
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl ProxyHandle {
    pub(crate) fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub(crate) async fn connection_snapshot(&self) -> ConnectionSnapshot {
        self.hybrid_pool.connection_snapshot().await
    }

    pub(crate) async fn stop(mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        let _ = self.task.await;
    }
}

#[derive(Debug)]
pub(crate) enum ProxyError {
    Bind(std::io::Error),
    Client(reqwest::Error),
    WebSocketClient(String),
    HealthCheck,
}

impl fmt::Display for ProxyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bind(error) => write!(formatter, "无法绑定 Turbo 回环端口：{error}"),
            Self::Client(error) => write!(formatter, "无法创建 Turbo HTTP 客户端：{error}"),
            Self::WebSocketClient(error) => {
                write!(formatter, "无法创建 Turbo WebSocket 客户端：{error}")
            }
            Self::HealthCheck => write!(formatter, "Turbo 本地服务健康检查失败"),
        }
    }
}

impl std::error::Error for ProxyError {}

#[derive(Clone, Debug)]
struct ProxyState {
    upstream: Url,
    compression_enabled: Arc<AtomicBool>,
    websocket_enabled: Arc<AtomicBool>,
    ai_cove_private_websocket_zstd: bool,
    metrics: Arc<Metrics>,
    client: reqwest::Client,
    websocket_client: WebSocketClient,
    hybrid_pool: hybrid_pool::HybridPool,
    max_request_body_bytes: usize,
}

type WebSocketClient = Client<HttpsConnector<HttpConnector>, Empty<Bytes>>;

pub(crate) async fn start_proxy(options: ProxyOptions) -> Result<ProxyHandle, ProxyError> {
    let listener = bind_preferred(&options.preferred_ports).await?;
    let address = listener.local_addr().map_err(ProxyError::Bind)?;
    let endpoint = format!("http://{address}/v1");
    let websocket_connector = HttpsConnectorBuilder::new()
        .with_native_roots()
        .map_err(|error| ProxyError::WebSocketClient(error.to_string()))?
        .https_or_http()
        .enable_http1()
        .build();
    let mut private_tls_config = rustls::ClientConfig::builder()
        .with_native_roots()
        .map_err(|error| ProxyError::WebSocketClient(error.to_string()))?
        .with_no_client_auth();
    private_tls_config.resumption =
        rustls::client::Resumption::in_memory_sessions(PRIVATE_TLS_SESSION_CACHE_SIZE);
    let private_tls_config = PrivateTlsConfig::new(Arc::new(private_tls_config));
    let hybrid_pool =
        hybrid_pool::HybridPool::new(private_tls_config.clone(), Arc::clone(&options.metrics));
    let state = ProxyState {
        upstream: options.upstream,
        compression_enabled: options.compression_enabled,
        websocket_enabled: options.websocket_enabled,
        ai_cove_private_websocket_zstd: options.ai_cove_private_websocket_zstd,
        metrics: options.metrics,
        client: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(ProxyError::Client)?,
        websocket_client: Client::builder(TokioExecutor::new()).build(websocket_connector),
        hybrid_pool: hybrid_pool.clone(),
        max_request_body_bytes: if options.max_request_body_bytes == 0 {
            DEFAULT_MAX_REQUEST_BODY_BYTES
        } else {
            options.max_request_body_bytes
        },
    };
    let app = Router::new()
        .route("/healthz", get(health))
        .fallback(proxy_request)
        .with_state(state);
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        let server = axum::serve(listener, app).with_graceful_shutdown(async move {
            let _ = shutdown_rx.await;
        });
        let _ = server.await;
    });

    if let Err(error) = wait_for_health(address).await {
        let _ = shutdown_tx.send(());
        let _ = task.await;
        return Err(error);
    }

    Ok(ProxyHandle {
        endpoint,
        hybrid_pool,
        shutdown: Some(shutdown_tx),
        task,
    })
}

async fn bind_preferred(ports: &[u16]) -> Result<TcpListener, ProxyError> {
    let mut last_error = None;
    let candidates = if ports.is_empty() { &[0][..] } else { ports };
    for port in candidates {
        let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), *port);
        match TcpListener::bind(address).await {
            Ok(listener) => return Ok(listener),
            Err(error) => last_error = Some(error),
        }
    }
    Err(ProxyError::Bind(last_error.unwrap_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::AddrNotAvailable, "no candidate ports")
    })))
}

async fn wait_for_health(address: SocketAddr) -> Result<(), ProxyError> {
    let health_url = format!("http://{address}/healthz");
    let client = reqwest::Client::new();
    for _ in 0..20 {
        match client.get(&health_url).send().await {
            Ok(response) if response.status().is_success() => return Ok(()),
            _ => tokio::time::sleep(Duration::from_millis(10)).await,
        }
    }
    Err(ProxyError::HealthCheck)
}

async fn health() -> impl IntoResponse {
    (
        StatusCode::OK,
        axum::Json(serde_json::json!({ "ok": true })),
    )
}

async fn proxy_request(
    State(state): State<ProxyState>,
    mut request: AxumRequest,
) -> Response<Body> {
    request.headers_mut().insert(
        HeaderName::from_static("x-ai-cove-client"),
        header::HeaderValue::from_static("turbo"),
    );
    request.headers_mut().insert(
        HeaderName::from_static("x-ai-cove-client-version"),
        header::HeaderValue::from_static(turbo_client_version()),
    );
    if is_websocket_upgrade(request.headers()) {
        return proxy_websocket(state, &mut request).await;
    }
    proxy_http(state, request, HttpTraffic::DIRECT).await
}

async fn proxy_http(
    state: ProxyState,
    request: AxumRequest,
    traffic: HttpTraffic,
) -> Response<Body> {
    let (parts, body) = request.into_parts();
    let path = parts.uri.path().to_owned();
    let raw_body = to_bytes(body, state.max_request_body_bytes).await;
    let Ok(raw_body) = raw_body else {
        state.metrics.record_http(HttpRequestMetric {
            path: &path,
            status: StatusCode::PAYLOAD_TOO_LARGE.as_u16(),
            raw_bytes: 0,
            sent_bytes: 0,
            compressed: false,
            result: traffic.result,
            route: traffic.route,
            failure_reason: Some("local request body limit exceeded"),
        });
        return json_error(StatusCode::PAYLOAD_TOO_LARGE, "request body too large");
    };
    let raw_len = raw_body.len();
    let should_compress = state.compression_enabled.load(Ordering::Relaxed)
        && is_compressible_json(&parts.method, &parts.headers);
    let (outbound_body, compressed) = if should_compress {
        match compress_if_smaller(raw_body.clone()).await {
            Ok(Some(compressed)) => (compressed, true),
            Ok(None) => (raw_body, false),
            Err(()) => {
                state.metrics.record_http(HttpRequestMetric {
                    path: &path,
                    status: StatusCode::INTERNAL_SERVER_ERROR.as_u16(),
                    raw_bytes: raw_len,
                    sent_bytes: 0,
                    compressed: false,
                    result: traffic.result,
                    route: traffic.route,
                    failure_reason: None,
                });
                return json_error(StatusCode::INTERNAL_SERVER_ERROR, "compression failed");
            }
        }
    } else {
        (raw_body, false)
    };
    let target = resolve_target(&state.upstream, &parts.uri);
    let mut upstream_request = state.client.request(parts.method.clone(), target);
    let hop_by_hop = hop_by_hop_headers(&parts.headers);
    for (name, value) in &parts.headers {
        if !hop_by_hop.contains(name) && *name != header::HOST && *name != header::CONTENT_LENGTH {
            upstream_request = upstream_request.header(name, value);
        }
    }
    if compressed {
        upstream_request = upstream_request.header(header::CONTENT_ENCODING, "zstd");
    }
    let sent_len = outbound_body.len();
    let Ok(upstream_response) = upstream_request.body(outbound_body).send().await else {
        state.metrics.record_http(HttpRequestMetric {
            path: &path,
            status: StatusCode::BAD_GATEWAY.as_u16(),
            raw_bytes: raw_len,
            sent_bytes: sent_len,
            compressed: false,
            result: traffic.result,
            route: traffic.route,
            failure_reason: None,
        });
        return json_error(StatusCode::BAD_GATEWAY, "upstream request failed");
    };
    let status = upstream_response.status();
    state.metrics.record_http(HttpRequestMetric {
        path: &path,
        status: status.as_u16(),
        raw_bytes: raw_len,
        sent_bytes: sent_len,
        compressed,
        result: traffic.result,
        route: traffic.route,
        failure_reason: is_context_length_exceeded(status.as_u16())
            .then_some("HTTP upstream returned status 413"),
    });
    let response_headers = upstream_response.headers().clone();
    let response_hop_by_hop = hop_by_hop_headers(&response_headers);
    let stream = upstream_response
        .bytes_stream()
        .map_err(std::io::Error::other);
    let mut response = Response::new(Body::from_stream(stream));
    *response.status_mut() = status;
    for (name, value) in &response_headers {
        if !response_hop_by_hop.contains(name) {
            response.headers_mut().append(name, value.clone());
        }
    }
    response
}

async fn proxy_websocket(state: ProxyState, request: &mut AxumRequest) -> Response<Body> {
    let path = request.uri().path().to_owned();
    if !state.websocket_enabled.load(Ordering::Relaxed) {
        state
            .metrics
            .record_websocket_error(&path, StatusCode::SERVICE_UNAVAILABLE.as_u16());
        return json_error(StatusCode::SERVICE_UNAVAILABLE, "websocket disabled");
    }
    let target = resolve_target(&state.upstream, request.uri());
    if state.ai_cove_private_websocket_zstd {
        let Ok(response) = client_upgrade_response(request.headers()) else {
            state
                .metrics
                .record_websocket_error(&path, StatusCode::BAD_REQUEST.as_u16());
            return json_error(StatusCode::BAD_REQUEST, "invalid websocket handshake");
        };
        hybrid::spawn(state, request, target, path);
        return response;
    }
    let Ok(target_uri) = target.as_str().parse() else {
        state
            .metrics
            .record_websocket_error(&path, StatusCode::BAD_GATEWAY.as_u16());
        return json_error(StatusCode::BAD_GATEWAY, "invalid websocket upstream");
    };
    let client_upgrade = hyper::upgrade::on(&mut *request);
    let outbound = build_websocket_request(request.method().clone(), request.headers(), target_uri);
    let Ok(mut upstream_response) = state.websocket_client.request(outbound).await else {
        state
            .metrics
            .record_websocket_error(&path, StatusCode::BAD_GATEWAY.as_u16());
        return json_error(StatusCode::BAD_GATEWAY, "websocket upstream failed");
    };
    if upstream_response.status() != StatusCode::SWITCHING_PROTOCOLS {
        state
            .metrics
            .record_websocket_error(&path, upstream_response.status().as_u16());
        let (parts, body) = upstream_response.into_parts();
        return Response::from_parts(parts, Body::new(body));
    }
    let upstream_upgrade = hyper::upgrade::on(&mut upstream_response);
    let mut response = Response::new(Body::empty());
    *response.status_mut() = upstream_response.status();
    *response.headers_mut() = upstream_response.headers().clone();
    let metrics = Arc::clone(&state.metrics);
    tokio::spawn(async move {
        let (Ok(client), Ok(upstream)) = tokio::join!(client_upgrade, upstream_upgrade) else {
            metrics.record_websocket_error(&path, StatusCode::INTERNAL_SERVER_ERROR.as_u16());
            return;
        };
        metrics.record_websocket_connected();
        let mut client = TokioIo::new(client);
        let mut upstream = TokioIo::new(upstream);
        match copy_bidirectional(&mut client, &mut upstream).await {
            Ok((client_to_upstream, _)) => {
                metrics.record_websocket_traffic(traffic::TrafficRecord {
                    timestamp_ms: traffic::now_ms(),
                    status: StatusCode::SWITCHING_PROTOCOLS.as_u16(),
                    path: &path,
                    raw_bytes: client_to_upstream,
                    sent_bytes: client_to_upstream,
                    transport: traffic::TrafficTransport::Ws,
                    result: traffic::TrafficResult::Success,
                    route: None,
                    failure_phase: None,
                    failure_reason: None,
                });
            }
            Err(_) => metrics.record_websocket_error(&path, 1011),
        }
        metrics.record_websocket_closed();
    });
    response
}

fn is_websocket_upgrade(headers: &HeaderMap) -> bool {
    let websocket = headers
        .get(header::UPGRADE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("websocket"));
    let connection_upgrade = headers
        .get(header::CONNECTION)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .any(|token| token.trim().eq_ignore_ascii_case("upgrade"))
        });
    websocket && connection_upgrade
}

fn build_websocket_request(
    method: Method,
    headers: &HeaderMap,
    target_uri: Uri,
) -> axum::http::Request<Empty<Bytes>> {
    let mut outbound = axum::http::Request::new(Empty::<Bytes>::new());
    *outbound.method_mut() = method;
    *outbound.uri_mut() = target_uri;
    *outbound.version_mut() = Version::HTTP_11;
    let hop_by_hop = hop_by_hop_headers(headers);
    for (name, value) in headers {
        if !hop_by_hop.contains(name) && *name != header::HOST && *name != header::CONTENT_LENGTH {
            outbound.headers_mut().append(name, value.clone());
        }
    }
    outbound.headers_mut().insert(
        header::CONNECTION,
        header::HeaderValue::from_static("upgrade"),
    );
    outbound.headers_mut().insert(
        header::UPGRADE,
        header::HeaderValue::from_static("websocket"),
    );
    outbound
}

fn is_compressible_json(method: &Method, headers: &HeaderMap) -> bool {
    if method != Method::POST || headers.contains_key(header::CONTENT_ENCODING) {
        return false;
    }
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("application/json"))
}

async fn compress_if_smaller(body: Bytes) -> Result<Option<Bytes>, ()> {
    if body.len() < MIN_COMPRESSION_INPUT_BYTES {
        return Ok(None);
    }
    let original_len = body.len();
    tokio::task::spawn_blocking(move || {
        zstd::stream::encode_all(Cursor::new(body), 3)
            .map(Bytes::from)
            .map(|compressed| (compressed.len() < original_len).then_some(compressed))
            .map_err(|_| ())
    })
    .await
    .map_err(|_| ())?
}

#[cfg(test)]
pub(crate) async fn measure_http_encoding(body: Bytes) -> Result<Option<Bytes>, ()> {
    compress_if_smaller(body).await
}

#[cfg(test)]
pub(crate) async fn measure_private_encoding(
    payload: Vec<u8>,
    original_binary: bool,
) -> Result<Vec<u8>, String> {
    encode_private_message_async(payload, original_binary)
        .await
        .map(|encoded| encoded.bytes)
        .map_err(|error| error.to_string())
}

fn resolve_target(upstream: &Url, uri: &axum::http::Uri) -> Url {
    let mut target = upstream.clone();
    let upstream_path = upstream.path().trim_end_matches('/');
    let incoming_path = uri.path();
    let target_path = if upstream_path.is_empty() || upstream_path == "/" {
        incoming_path.to_owned()
    } else if let Some(suffix) = incoming_path.strip_prefix("/v1") {
        format!("{upstream_path}{suffix}")
    } else {
        incoming_path.to_owned()
    };
    target.set_path(&target_path);
    target.set_query(uri.query());
    target
}

fn hop_by_hop_headers(headers: &HeaderMap) -> HashSet<HeaderName> {
    let mut names = HOP_BY_HOP_HEADERS
        .iter()
        .filter_map(|name| HeaderName::from_bytes(name.as_bytes()).ok())
        .collect::<HashSet<_>>();
    if let Some(connection) = headers
        .get(header::CONNECTION)
        .and_then(|value| value.to_str().ok())
    {
        for name in connection
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            if let Ok(header_name) = HeaderName::from_bytes(name.as_bytes()) {
                names.insert(header_name);
            }
        }
    }
    names
}

fn json_error(status: StatusCode, message: &str) -> Response<Body> {
    let body = serde_json::json!({ "error": message }).to_string();
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/json; charset=utf-8"),
    );
    response
}

#[cfg(test)]
mod tests {
    use std::{
        convert::Infallible,
        error::Error,
        io::Cursor,
        process::{Command, Stdio},
        sync::Arc,
        time::Duration,
    };

    use axum::{
        Router,
        body::{Body, Bytes},
        extract::State,
        http::{HeaderMap, StatusCode},
        response::Response,
        routing::post,
    };
    use futures_util::{SinkExt, StreamExt};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
        sync::{Mutex, oneshot},
    };
    use tokio_tungstenite::{
        WebSocketStream, connect_async,
        tungstenite::{
            client::IntoClientRequest,
            handshake::derive_accept_key,
            protocol::{
                Message as WebSocketMessage, Role,
                frame::{
                    Frame,
                    coding::{CloseCode, Data, OpCode},
                },
            },
        },
    };
    use url::Url;

    use super::*;

    type CapturedRequest = Arc<Mutex<Option<(HeaderMap, Bytes)>>>;
    type CapturedPrivateMessage = (Vec<u8>, bool, bool);
    type CapturedPrivateMessages = (CapturedPrivateMessage, CapturedPrivateMessage);

    #[test]
    fn one_http_outcome_updates_atomic_event_and_persisted_route_once() -> Result<(), Box<dyn Error>>
    {
        // Given: one direct HTTP outcome and no route side-channel write.
        let root = tempfile::tempdir()?;
        let path = root.path().join("traffic.jsonl");
        let metrics = Metrics::default();

        // When: the existing Metrics seam records the completed outcome once.
        metrics.record_http(HttpRequestMetric {
            path: "/v1/responses",
            status: 201,
            raw_bytes: 100,
            sent_bytes: 50,
            compressed: true,
            result: traffic::TrafficResult::Success,
            route: traffic::TrafficRoute::DirectHttp,
            failure_reason: None,
        });
        metrics.save_traffic(&path)?;

        // Then: atomics, recent traffic, and persistence agree on one outcome.
        let live = metrics.snapshot();
        assert_eq!(live.requests, 1);
        assert_eq!(live.direct_http, 1);
        assert_eq!(metrics.traffic_snapshot().recent_requests.len(), 1);
        assert_eq!(Metrics::load_traffic(&path).snapshot().direct_http, 1);

        Ok(())
    }

    #[test]
    fn only_successful_responses_advance_activation_evidence() {
        let metrics = Metrics::default();

        metrics.record_http(HttpRequestMetric {
            path: "/v1/responses",
            status: 200,
            raw_bytes: 100,
            sent_bytes: 50,
            compressed: true,
            result: traffic::TrafficResult::Success,
            route: traffic::TrafficRoute::DirectHttp,
            failure_reason: None,
        });
        metrics.record_http(HttpRequestMetric {
            path: "/v1/responses",
            status: 502,
            raw_bytes: 100,
            sent_bytes: 50,
            compressed: false,
            result: traffic::TrafficResult::Success,
            route: traffic::TrafficRoute::DirectHttp,
            failure_reason: None,
        });
        metrics.record_http(HttpRequestMetric {
            path: "/v1/models",
            status: 200,
            raw_bytes: 100,
            sent_bytes: 50,
            compressed: false,
            result: traffic::TrafficResult::Success,
            route: traffic::TrafficRoute::DirectHttp,
            failure_reason: None,
        });
        metrics.record_http(HttpRequestMetric {
            path: "/v1/responses",
            status: 200,
            raw_bytes: 100,
            sent_bytes: 50,
            compressed: false,
            result: traffic::TrafficResult::Error,
            route: traffic::TrafficRoute::DirectHttp,
            failure_reason: None,
        });
        metrics.record_websocket_connected();
        metrics.record_websocket_outcome(
            traffic::TrafficRecord {
                timestamp_ms: traffic::now_ms(),
                status: 1011,
                path: "/v1/responses",
                raw_bytes: 100,
                sent_bytes: 50,
                transport: traffic::TrafficTransport::Ws,
                result: traffic::TrafficResult::Error,
                route: Some(traffic::TrafficRoute::HybridWs),
                failure_phase: Some(traffic::FailurePhase::HybridActive),
                failure_reason: Some("upstream failed after send"),
            },
            true,
        );

        assert_eq!(metrics.snapshot().successful_responses, 1);

        metrics.record_websocket_outcome(
            traffic::TrafficRecord {
                timestamp_ms: traffic::now_ms(),
                status: StatusCode::SWITCHING_PROTOCOLS.as_u16(),
                path: "/v1/responses",
                raw_bytes: 100,
                sent_bytes: 50,
                transport: traffic::TrafficTransport::Ws,
                result: traffic::TrafficResult::Success,
                route: Some(traffic::TrafficRoute::HybridWs),
                failure_phase: None,
                failure_reason: None,
            },
            true,
        );

        assert_eq!(metrics.snapshot().successful_responses, 2);
    }

    #[test]
    fn hybrid_http_413_records_active_failure_phase() -> Result<(), Box<dyn Error>> {
        for traffic in [HttpTraffic::HYBRID_COLD_START, HttpTraffic::HYBRID_RECOVERY] {
            let metrics = Metrics::default();
            metrics.record_http(HttpRequestMetric {
                path: "/v1/responses",
                status: 413,
                raw_bytes: 100,
                sent_bytes: 50,
                compressed: false,
                result: traffic.result,
                route: traffic.route,
                failure_reason: Some("HTTP upstream returned status 413"),
            });

            let event = metrics
                .traffic_snapshot()
                .recent_requests
                .into_iter()
                .next()
                .ok_or("Hybrid HTTP 413 traffic event missing")?;
            let event = serde_json::to_value(event)?;
            assert_eq!(event.get("result"), Some(&serde_json::json!("error")));
            assert_eq!(
                event.get("failurePhase"),
                Some(&serde_json::json!("hybridActive"))
            );
            assert_eq!(
                event.get("failureReason"),
                Some(&serde_json::json!("HTTP upstream returned status 413"))
            );
        }
        Ok(())
    }

    #[test]
    fn hybrid_http_success_outcomes_preserve_route_classification() -> Result<(), Box<dyn Error>> {
        for (traffic, cold_start, recovery, fallbacks) in [
            (HttpTraffic::HYBRID_COLD_START, 1, 0, 0),
            (HttpTraffic::HYBRID_RECOVERY, 0, 1, 1),
        ] {
            // Given: one successful Hybrid HTTP outcome.
            let metrics = Metrics::default();

            // When: the final outcome crosses the Metrics seam once.
            metrics.record_http(HttpRequestMetric {
                path: "/v1/responses",
                status: 201,
                raw_bytes: 100,
                sent_bytes: 50,
                compressed: false,
                result: traffic.result,
                route: traffic.route,
                failure_reason: None,
            });

            // Then: its existing route and fallback classification remain intact.
            let snapshot = metrics.snapshot();
            assert_eq!(snapshot.requests, 1);
            assert_eq!(snapshot.hybrid_cold_start_http, cold_start);
            assert_eq!(snapshot.hybrid_recovery_http, recovery);
            assert_eq!(snapshot.http_fallbacks, fallbacks);
            let event = metrics
                .traffic_snapshot()
                .recent_requests
                .into_iter()
                .next()
                .ok_or("Hybrid HTTP traffic event missing")?;
            let event = serde_json::to_value(event)?;
            assert_eq!(
                event.get("result"),
                Some(&serde_json::to_value(traffic.result)?)
            );
            assert_eq!(
                event.get("route"),
                Some(&serde_json::to_value(traffic.route)?)
            );
        }
        Ok(())
    }

    #[test]
    fn one_hybrid_ws_active_failure_records_one_final_outcome() -> Result<(), Box<dyn Error>> {
        // Given: one sent Hybrid WS request that later fails while active.
        let root = tempfile::tempdir()?;
        let path = root.path().join("traffic.jsonl");
        let metrics = Metrics::default();

        // When: its final outcome crosses the Metrics seam once.
        metrics.record_websocket_outcome(
            traffic::TrafficRecord {
                timestamp_ms: traffic::now_ms(),
                status: 1011,
                path: "/v1/responses",
                raw_bytes: 100,
                sent_bytes: 50,
                transport: traffic::TrafficTransport::Ws,
                result: traffic::TrafficResult::Error,
                route: Some(traffic::TrafficRoute::HybridWs),
                failure_phase: Some(traffic::FailurePhase::HybridActive),
                failure_reason: Some("upstream failed after send"),
            },
            true,
        );
        metrics.save_traffic(&path)?;

        // Then: message, failure, event, and persisted route each advance once.
        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.websocket_messages, 1);
        assert_eq!(snapshot.websocket_failures, 1);
        assert_eq!(snapshot.websocket_raw_bytes, 100);
        assert_eq!(snapshot.websocket_sent_bytes, 50);
        assert!(snapshot.websocket_zstd_verified);
        assert_eq!(snapshot.hybrid_ws, 1);
        let events = metrics.traffic_snapshot().recent_requests;
        assert_eq!(events.len(), 1);
        let event = serde_json::to_value(events.into_iter().next())?;
        assert_eq!(event.get("result"), Some(&serde_json::json!("error")));
        assert_eq!(
            event.get("failurePhase"),
            Some(&serde_json::json!("hybridActive"))
        );
        assert_eq!(Metrics::load_traffic(&path).snapshot().hybrid_ws, 1);
        Ok(())
    }

    async fn read_http_head(stream: &mut TcpStream) -> Result<Vec<u8>, std::io::Error> {
        let mut head = Vec::new();
        let mut byte = [0_u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            stream.read_exact(&mut byte).await?;
            head.push(byte[0]);
        }
        Ok(head)
    }

    async fn accept_test_websocket(
        mut stream: TcpStream,
        subprotocol: Option<&str>,
    ) -> Option<(WebSocketStream<TcpStream>, String)> {
        let head = String::from_utf8(read_http_head(&mut stream).await.ok()?).ok()?;
        let key = http_header_value(&head, "Sec-WebSocket-Key")?;
        let accept = derive_accept_key(key.as_bytes());
        let protocol = subprotocol.map_or_else(String::new, |protocol| {
            format!("Sec-WebSocket-Protocol: {protocol}\r\n")
        });
        let response = format!(
            "HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Accept: {accept}\r\n{protocol}\r\n"
        );
        stream.write_all(response.as_bytes()).await.ok()?;
        let websocket = WebSocketStream::from_raw_socket(stream, Role::Server, None).await;
        Some((websocket, head))
    }

    fn http_header_value<'a>(head: &'a str, expected_name: &str) -> Option<&'a str> {
        head.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case(expected_name)
                .then(|| value.trim())
        })
    }

    async fn private_echo_upstream(
        listener: TcpListener,
        headers_tx: oneshot::Sender<String>,
        messages_tx: oneshot::Sender<CapturedPrivateMessages>,
    ) {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let Some((mut websocket, head)) =
            accept_test_websocket(stream, Some(PRIVATE_WEBSOCKET_SUBPROTOCOL)).await
        else {
            return;
        };
        let _ = headers_tx.send(head);
        let Some(Ok(WebSocketMessage::Binary(text_envelope))) = websocket.next().await else {
            return;
        };
        let Ok(text) = decode_private_message(&text_envelope) else {
            return;
        };
        let Ok(text_response) =
            encode_private_message(b"response response response response", false)
        else {
            return;
        };
        if websocket
            .send(WebSocketMessage::Binary(Bytes::from(text_response)))
            .await
            .is_err()
        {
            return;
        }
        let Some(Ok(WebSocketMessage::Binary(binary_envelope))) = websocket.next().await else {
            return;
        };
        let Ok(binary) = decode_private_message(&binary_envelope) else {
            return;
        };
        let Ok(binary_response) = encode_private_message(&binary.payload, true) else {
            return;
        };
        if websocket
            .send(WebSocketMessage::Binary(Bytes::from(binary_response)))
            .await
            .is_err()
        {
            return;
        }
        let _ = messages_tx.send((
            (text.payload, text.original_binary, text.compressed),
            (binary.payload, binary.original_binary, binary.compressed),
        ));
        let _ = websocket.next().await;
    }

    async fn private_rejection_upstream(
        listener: TcpListener,
        private_headers_tx: oneshot::Sender<String>,
        fallback_headers_tx: oneshot::Sender<String>,
    ) {
        let mut private_headers_tx = Some(private_headers_tx);
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let Some((mut websocket, head)) = accept_test_websocket(stream, None).await else {
                return;
            };
            if http_header_value(&head, "Sec-WebSocket-Protocol")
                == Some(PRIVATE_WEBSOCKET_SUBPROTOCOL)
            {
                if let Some(sender) = private_headers_tx.take() {
                    let _ = sender.send(head);
                }
                let _ = websocket.next().await;
                continue;
            }
            let _ = fallback_headers_tx.send(head);
            if let Some(Ok(message)) = websocket.next().await {
                let _ = websocket.send(message).await;
            }
            return;
        }
    }

    async fn private_invalid_frame_upstream(listener: TcpListener) {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let Some((mut websocket, _)) =
            accept_test_websocket(stream, Some(PRIVATE_WEBSOCKET_SUBPROTOCOL)).await
        else {
            return;
        };
        let _ = websocket
            .send(WebSocketMessage::Text("invalid private frame".into()))
            .await;
        let _ = websocket.next().await;
    }

    async fn private_fragment_upstream(
        listener: TcpListener,
        captured_tx: oneshot::Sender<Vec<u8>>,
    ) {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let Some((mut websocket, _)) =
            accept_test_websocket(stream, Some(PRIVATE_WEBSOCKET_SUBPROTOCOL)).await
        else {
            return;
        };
        let Some(Ok(WebSocketMessage::Binary(envelope))) = websocket.next().await else {
            return;
        };
        let Ok(decoded) = decode_private_message(&envelope) else {
            return;
        };
        let _ = captured_tx.send(decoded.payload);
        let Ok(response) = encode_private_message(b"fragment", false) else {
            return;
        };
        let _ = websocket
            .send(WebSocketMessage::Binary(Bytes::from(response)))
            .await;
        let _ = websocket.next().await;
    }

    async fn private_control_upstream(
        listener: TcpListener,
        start_rx: oneshot::Receiver<()>,
        captured_tx: oneshot::Sender<Bytes>,
    ) {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let Some((mut websocket, _)) =
            accept_test_websocket(stream, Some(PRIVATE_WEBSOCKET_SUBPROTOCOL)).await
        else {
            return;
        };
        let Some(Ok(WebSocketMessage::Binary(_))) = websocket.next().await else {
            return;
        };
        if start_rx.await.is_err() {
            return;
        }
        let ping = Bytes::from_static(b"upstream-ping");
        if websocket
            .send(WebSocketMessage::Ping(ping.clone()))
            .await
            .is_err()
        {
            return;
        }
        let Ok(Some(Ok(WebSocketMessage::Pong(payload)))) =
            tokio::time::timeout(Duration::from_secs(1), websocket.next()).await
        else {
            return;
        };
        let _ = captured_tx.send(payload);
        let _ = websocket.next().await;
    }

    async fn upstream(
        State(captured): State<CapturedRequest>,
        headers: HeaderMap,
        body: Bytes,
    ) -> Response<Body> {
        *captured.lock().await = Some((headers, body));
        Response::builder()
            .status(StatusCode::CREATED)
            .header("content-type", "text/event-stream")
            .body(Body::from("data: ok\n\n"))
            .unwrap_or_else(|_| Response::new(Body::empty()))
    }

    async fn streaming_upstream() -> Response<Body> {
        let stream = futures_util::stream::unfold(0_u8, |state| async move {
            match state {
                0 => Some((
                    Ok::<Bytes, Infallible>(Bytes::from_static(b"data: first\n\n")),
                    1,
                )),
                1 => {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    Some((
                        Ok::<Bytes, Infallible>(Bytes::from_static(b"data: second\n\n")),
                        2,
                    ))
                }
                _ => None,
            }
        });
        let mut response = Response::new(Body::from_stream(stream));
        response.headers_mut().insert(
            "content-type",
            "text/event-stream"
                .parse()
                .unwrap_or_else(|_| axum::http::HeaderValue::from_static("text/plain")),
        );
        response
    }

    #[tokio::test]
    async fn public_http_entry_compresses_json_and_records_real_metrics()
    -> Result<(), Box<dyn Error>> {
        let captured = CapturedRequest::default();
        let upstream_listener = TcpListener::bind("127.0.0.1:0").await?;
        let upstream_address = upstream_listener.local_addr()?;
        let upstream_app = Router::new()
            .route("/v1/responses", post(upstream))
            .with_state(Arc::clone(&captured));
        let upstream_task = tokio::spawn(async move {
            let _ = axum::serve(upstream_listener, upstream_app).await;
        });

        let compression_enabled = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let metrics = Arc::new(Metrics::default());
        let proxy = start_proxy(ProxyOptions {
            upstream: Url::parse(&format!("http://{upstream_address}/v1"))?,
            compression_enabled,
            websocket_enabled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            ai_cove_private_websocket_zstd: false,
            metrics: Arc::clone(&metrics),
            preferred_ports: vec![0],
            max_request_body_bytes: 128 * 1024 * 1024,
        })
        .await?;
        let input = serde_json::json!({
            "model": "gpt-5.6-luna",
            "input": "repeat ".repeat(3000)
        })
        .to_string();

        let response = reqwest::Client::new()
            .post(format!("{}/responses", proxy.endpoint()))
            .header("content-type", "application/json")
            .header("authorization", "Bearer test-only")
            .body(input.clone())
            .send()
            .await?;

        assert_eq!(response.status(), StatusCode::CREATED);
        assert_eq!(response.text().await?, "data: ok\n\n");
        let (headers, compressed) = captured
            .lock()
            .await
            .take()
            .ok_or("upstream request missing")?;
        assert_eq!(
            headers
                .get("content-encoding")
                .and_then(|v| v.to_str().ok()),
            Some("zstd")
        );
        assert_eq!(
            headers.get("authorization").and_then(|v| v.to_str().ok()),
            Some("Bearer test-only")
        );
        assert_eq!(
            headers
                .get("x-ai-cove-client")
                .and_then(|v| v.to_str().ok()),
            Some("turbo")
        );
        assert_eq!(
            headers
                .get("x-ai-cove-client-version")
                .and_then(|v| v.to_str().ok()),
            Some(turbo_client_version())
        );
        let decoded = zstd::stream::decode_all(Cursor::new(compressed))?;
        assert_eq!(decoded, input.as_bytes());

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.requests, 1);
        assert_eq!(snapshot.direct_http, 1);
        assert_eq!(snapshot.hybrid_ws, 0);
        assert_eq!(snapshot.hybrid_cold_start_http, 0);
        assert_eq!(snapshot.hybrid_recovery_http, 0);
        assert_eq!(snapshot.raw_bytes, input.len() as u64);
        assert!(snapshot.sent_bytes < snapshot.raw_bytes);
        assert!(snapshot.compression_verified);
        let event = metrics
            .traffic_snapshot()
            .recent_requests
            .into_iter()
            .next()
            .ok_or("HTTP traffic event missing")?;
        let event = serde_json::to_value(event)?;
        assert_eq!(
            event.get("status").and_then(serde_json::Value::as_u64),
            Some(201)
        );
        assert_eq!(
            event.get("result").and_then(serde_json::Value::as_str),
            Some("success")
        );
        assert_eq!(
            event.get("route").and_then(serde_json::Value::as_str),
            Some("directHttp")
        );

        proxy.stop().await;
        upstream_task.abort();
        Ok(())
    }

    #[tokio::test]
    async fn failed_upstream_does_not_verify_compression() -> Result<(), Box<dyn Error>> {
        let unavailable = TcpListener::bind("127.0.0.1:0").await?;
        let upstream_address = unavailable.local_addr()?;
        drop(unavailable);
        let metrics = Arc::new(Metrics::default());
        let proxy = start_proxy(ProxyOptions {
            upstream: Url::parse(&format!("http://{upstream_address}/v1"))?,
            compression_enabled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            websocket_enabled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            ai_cove_private_websocket_zstd: false,
            metrics: Arc::clone(&metrics),
            preferred_ports: vec![0],
            max_request_body_bytes: 1024 * 1024,
        })
        .await?;
        let input = serde_json::json!({ "input": "repeat ".repeat(2000) }).to_string();

        let response = reqwest::Client::new()
            .post(format!("{}/responses", proxy.endpoint()))
            .header("content-type", "application/json")
            .body(input.clone())
            .send()
            .await?;

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.requests, 1);
        assert!(!snapshot.compression_verified);
        let event = metrics
            .traffic_snapshot()
            .recent_requests
            .into_iter()
            .next()
            .ok_or("failed HTTP traffic event missing")?;
        let event = serde_json::to_value(event)?;
        assert_eq!(
            event.get("status").and_then(serde_json::Value::as_u64),
            Some(502)
        );
        assert_eq!(
            event.get("result").and_then(serde_json::Value::as_str),
            Some("error")
        );
        assert_eq!(
            event.get("rawBytes").and_then(serde_json::Value::as_u64),
            Some(input.len() as u64)
        );
        let sent_bytes = event
            .get("sentBytes")
            .and_then(serde_json::Value::as_u64)
            .ok_or("failed HTTP sent bytes missing")?;
        assert!(sent_bytes > 0);
        assert!(sent_bytes < input.len() as u64);
        proxy.stop().await;
        Ok(())
    }

    #[test]
    fn private_zstd_v1_exact_vectors_round_trip() -> Result<(), Box<dyn Error>> {
        const TEXT_OK: &[u8] = b"AICZ\x01\x00\x00\x00\x00\x02ok";
        const BINARY_BYTES: &[u8] = b"AICZ\x01\x02\x00\x00\x00\x02\x00\xff";

        assert_eq!(PRIVATE_WEBSOCKET_SUBPROTOCOL, "ai-cove-zstd.v1");
        assert_eq!(encode_private_message(b"ok", false)?, TEXT_OK);
        assert_eq!(encode_private_message(&[0, 0xff], true)?, BINARY_BYTES);

        let text = decode_private_message(TEXT_OK)?;
        assert_eq!(text.payload, b"ok");
        assert!(!text.original_binary);
        assert!(!text.compressed);

        let binary = decode_private_message(BINARY_BYTES)?;
        assert_eq!(binary.payload, [0, 0xff]);
        assert!(binary.original_binary);
        assert!(!binary.compressed);
        Ok(())
    }

    #[test]
    fn private_zstd_v1_decodes_cross_language_vector() -> Result<(), Box<dyn Error>> {
        const ENVELOPE: &[u8] = b"AICZ\x01\x01\x00\x00\x25\x00\x28\xb5\x2f\xfd\x60\x00\x24\x7d\x01\x00\x54\x02\x7b\x22\x74\x79\x70\x65\x22\x3a\x22\x72\x65\x73\x70\x6f\x6e\x73\x65\x2e\x63\x72\x65\x61\x74\x65\x22\x2c\x22\x69\x6e\x70\x75\x74\x22\x3a\x5b\x5d\x7d\x01\x54\x16\x05\x31\xc5\x26\x28";
        let expected = br#"{"type":"response.create","input":[]}"#.repeat(256);

        let decoded = decode_private_message(ENVELOPE)?;

        assert_eq!(decoded.payload, expected);
        assert!(!decoded.original_binary);
        assert!(decoded.compressed);
        Ok(())
    }

    #[test]
    fn private_zstd_v1_compresses_only_when_smaller() -> Result<(), Box<dyn Error>> {
        let original = b"repeat repeat repeat repeat repeat repeat repeat repeat".repeat(512);

        let encoded = encode_private_message(&original, false)?;

        assert_eq!(encoded.get(..4), Some(b"AICZ".as_slice()));
        assert_eq!(encoded.get(4), Some(&1));
        assert_eq!(encoded.get(5), Some(&1));
        assert!(encoded.len() - PRIVATE_ENVELOPE_HEADER_BYTES < original.len());
        let decoded = decode_private_message(&encoded)?;
        assert_eq!(decoded.payload, original);
        assert!(decoded.compressed);
        Ok(())
    }

    #[test]
    fn private_zstd_v1_rejects_invalid_vectors_with_contract_close_codes() {
        let cases: [(&[u8], u16); 8] = [
            (b"AIC", 1002),
            (b"NOPE\x01\x00\x00\x00\x00\x00", 1002),
            (b"AICZ\x02\x00\x00\x00\x00\x00", 1002),
            (b"AICZ\x01\x04\x00\x00\x00\x00", 1002),
            (b"AICZ\x01\x00\x08\x00\x00\x01", 1009),
            (b"AICZ\x01\x00\x00\x00\x00\x01\xff", 1007),
            (b"AICZ\x01\x01\x00\x00\x00\x01\x00", 1002),
            (b"AICZ\x01\x01\x00\x00\x00\x64\x00", 1007),
        ];

        for (vector, expected_code) in cases {
            let error = decode_private_message(vector).expect_err("vector must fail");
            assert_eq!(error.close_code, expected_code);
        }
    }

    #[test]
    fn private_zstd_v1_reports_decoded_length_mismatch_as_invalid_data()
    -> Result<(), Box<dyn Error>> {
        let original = b"a".repeat(64);
        let compressed = zstd::stream::encode_all(Cursor::new(&original), 3)?;
        let declared_len = u32::try_from(original.len() + 1)?;
        let mut envelope = b"AICZ\x01\x01".to_vec();
        envelope.extend_from_slice(&declared_len.to_be_bytes());
        envelope.extend_from_slice(&compressed);

        let error = decode_private_message(&envelope).expect_err("length mismatch must fail");

        assert_eq!(error.close_code, 1007);
        Ok(())
    }

    #[test]
    fn private_zstd_v1_rejects_window_above_message_limit() {
        const OVERSIZED_WINDOW_FRAME: &[u8] =
            &[0x28, 0xb5, 0x2f, 0xfd, 0x00, 0x90, 0x23, 0x03, 0x00, b'x'];
        let mut envelope = b"AICZ\x01\x01\x00\x00\x00\x64".to_vec();
        envelope.extend_from_slice(OVERSIZED_WINDOW_FRAME);

        let error = decode_private_message(&envelope).expect_err("oversized window must fail");

        assert_eq!(error.close_code, 1007);
        assert_eq!(error.to_string(), "zstd payload is damaged");
    }

    #[tokio::test]
    async fn compression_off_keeps_body_and_filters_connection_headers()
    -> Result<(), Box<dyn Error>> {
        let captured = CapturedRequest::default();
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let app = Router::new()
            .route("/v1/responses", post(upstream))
            .with_state(Arc::clone(&captured));
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let proxy = start_proxy(ProxyOptions {
            upstream: Url::parse(&format!("http://{address}/v1"))?,
            compression_enabled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            websocket_enabled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            ai_cove_private_websocket_zstd: false,
            metrics: Arc::new(Metrics::default()),
            preferred_ports: vec![0],
            max_request_body_bytes: 1024,
        })
        .await?;
        let input = r#"{"input":"plain"}"#;

        reqwest::Client::new()
            .post(format!("{}/responses", proxy.endpoint()))
            .header("content-type", "application/json")
            .header("connection", "x-remove")
            .header("x-remove", "secret-hop")
            .body(input)
            .send()
            .await?;

        let (headers, body) = captured
            .lock()
            .await
            .take()
            .ok_or("upstream request missing")?;
        assert_eq!(body, input);
        assert!(!headers.contains_key("content-encoding"));
        assert!(!headers.contains_key("connection"));
        assert!(!headers.contains_key("x-remove"));
        proxy.stop().await;
        task.abort();
        Ok(())
    }

    #[tokio::test]
    async fn request_limit_returns_413_before_reaching_upstream() -> Result<(), Box<dyn Error>> {
        let captured = CapturedRequest::default();
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let app = Router::new()
            .route("/v1/responses", post(upstream))
            .with_state(Arc::clone(&captured));
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let metrics = Arc::new(Metrics::default());
        let proxy = start_proxy(ProxyOptions {
            upstream: Url::parse(&format!("http://{address}/v1"))?,
            compression_enabled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            websocket_enabled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            ai_cove_private_websocket_zstd: false,
            metrics: Arc::clone(&metrics),
            preferred_ports: vec![0],
            max_request_body_bytes: 4,
        })
        .await?;

        let response = reqwest::Client::new()
            .post(format!("{}/responses", proxy.endpoint()))
            .header("content-type", "application/json")
            .body("12345")
            .send()
            .await?;

        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert!(captured.lock().await.is_none());
        let event = metrics
            .traffic_snapshot()
            .recent_requests
            .into_iter()
            .next()
            .ok_or("413 traffic event missing")?;
        let event = serde_json::to_value(event)?;
        assert_eq!(
            event.get("status").and_then(serde_json::Value::as_u64),
            Some(413)
        );
        assert_eq!(
            event.get("result").and_then(serde_json::Value::as_str),
            Some("error")
        );
        assert_eq!(
            event
                .get("failureReason")
                .and_then(serde_json::Value::as_str),
            Some("local request body limit exceeded")
        );
        proxy.stop().await;
        task.abort();
        Ok(())
    }

    #[tokio::test]
    async fn sse_first_chunk_is_forwarded_without_full_response_buffering()
    -> Result<(), Box<dyn Error>> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let app = Router::new().route("/v1/responses", post(streaming_upstream));
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let proxy = start_proxy(ProxyOptions {
            upstream: Url::parse(&format!("http://{address}/v1"))?,
            compression_enabled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            websocket_enabled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            ai_cove_private_websocket_zstd: false,
            metrics: Arc::new(Metrics::default()),
            preferred_ports: vec![0],
            max_request_body_bytes: 1024,
        })
        .await?;
        let response = reqwest::Client::new()
            .post(format!("{}/responses", proxy.endpoint()))
            .body("{}")
            .send()
            .await?;
        let mut stream = response.bytes_stream();
        let first = tokio::time::timeout(Duration::from_millis(100), stream.next())
            .await?
            .ok_or("missing first SSE chunk")??;
        assert_eq!(first, "data: first\n\n");

        proxy.stop().await;
        task.abort();
        Ok(())
    }

    #[tokio::test]
    async fn private_websocket_zstd_translates_messages_without_deflate()
    -> Result<(), Box<dyn Error>> {
        let upstream_listener = TcpListener::bind("127.0.0.1:0").await?;
        let upstream_address = upstream_listener.local_addr()?;
        let (headers_tx, headers_rx) = oneshot::channel();
        let (messages_tx, messages_rx) = oneshot::channel();
        let upstream_task = tokio::spawn(private_echo_upstream(
            upstream_listener,
            headers_tx,
            messages_tx,
        ));
        let metrics = Arc::new(Metrics::default());
        let proxy = start_proxy(ProxyOptions {
            upstream: Url::parse(&format!("http://{upstream_address}/v1"))?,
            compression_enabled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            websocket_enabled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            ai_cove_private_websocket_zstd: true,
            metrics: Arc::clone(&metrics),
            preferred_ports: vec![0],
            max_request_body_bytes: 1024,
        })
        .await?;
        let endpoint = Url::parse(proxy.endpoint())?;
        let mut request = format!(
            "ws://127.0.0.1:{}/v1/responses",
            endpoint.port().ok_or("missing port")?
        )
        .into_client_request()?;
        request.headers_mut().insert(
            header::SEC_WEBSOCKET_EXTENSIONS,
            header::HeaderValue::from_static("permessage-deflate"),
        );
        request.headers_mut().insert(
            header::AUTHORIZATION,
            header::HeaderValue::from_static("Bearer test-only"),
        );

        let (mut client, response) = connect_async(request).await?;
        assert!(
            !response
                .headers()
                .contains_key(header::SEC_WEBSOCKET_EXTENSIONS)
        );
        assert!(
            !response
                .headers()
                .contains_key(header::SEC_WEBSOCKET_PROTOCOL)
        );
        let text_input = "request ".repeat(4096);
        client
            .send(WebSocketMessage::Text(text_input.clone().into()))
            .await?;
        assert_eq!(
            client.next().await.ok_or("missing text response")??,
            WebSocketMessage::Text("response response response response".into())
        );
        client
            .send(WebSocketMessage::Binary(Bytes::from_static(&[0, 0xff])))
            .await?;
        assert_eq!(
            client.next().await.ok_or("missing binary response")??,
            WebSocketMessage::Binary(Bytes::from_static(&[0, 0xff]))
        );
        client.close(None).await?;

        let upstream_head = headers_rx.await?;
        assert_eq!(
            http_header_value(&upstream_head, "Sec-WebSocket-Extensions"),
            None
        );
        assert_eq!(
            http_header_value(&upstream_head, "Sec-WebSocket-Protocol"),
            Some(PRIVATE_WEBSOCKET_SUBPROTOCOL)
        );
        assert_eq!(
            http_header_value(&upstream_head, "Authorization"),
            Some("Bearer test-only")
        );
        assert_eq!(
            http_header_value(&upstream_head, "X-AI-Cove-Client"),
            Some("turbo")
        );
        assert_eq!(
            http_header_value(&upstream_head, "X-AI-Cove-Client-Version"),
            Some(turbo_client_version())
        );
        let (text, binary) = messages_rx.await?;
        assert_eq!(text.0, text_input.as_bytes());
        assert!(!text.1);
        assert!(text.2);
        assert_eq!(binary.0, [0, 0xff]);
        assert!(binary.1);
        assert!(!binary.2);
        let snapshot = metrics.snapshot();
        assert!(snapshot.websocket_zstd_verified);
        assert_eq!(snapshot.websocket_messages, 2);
        assert!(snapshot.websocket_sent_bytes < snapshot.websocket_raw_bytes);
        assert_eq!(snapshot.websocket_failures, 0);

        proxy.stop().await;
        upstream_task.abort();
        Ok(())
    }

    #[tokio::test]
    async fn private_subprotocol_rejection_reconnects_transparently_with_deflate()
    -> Result<(), Box<dyn Error>> {
        let upstream_listener = TcpListener::bind("127.0.0.1:0").await?;
        let upstream_address = upstream_listener.local_addr()?;
        let (private_headers_tx, private_headers_rx) = oneshot::channel();
        let (fallback_headers_tx, fallback_headers_rx) = oneshot::channel();
        let upstream_task = tokio::spawn(private_rejection_upstream(
            upstream_listener,
            private_headers_tx,
            fallback_headers_tx,
        ));
        let metrics = Arc::new(Metrics::default());
        let proxy = start_proxy(ProxyOptions {
            upstream: Url::parse(&format!("http://{upstream_address}/v1"))?,
            compression_enabled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            websocket_enabled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            ai_cove_private_websocket_zstd: true,
            metrics: Arc::clone(&metrics),
            preferred_ports: vec![0],
            max_request_body_bytes: 1024,
        })
        .await?;
        let endpoint = Url::parse(proxy.endpoint())?;
        let mut request = format!(
            "ws://127.0.0.1:{}/v1/responses",
            endpoint.port().ok_or("missing port")?
        )
        .into_client_request()?;
        request.headers_mut().insert(
            header::SEC_WEBSOCKET_EXTENSIONS,
            header::HeaderValue::from_static("permessage-deflate"),
        );
        request.headers_mut().insert(
            header::CONNECTION,
            header::HeaderValue::from_static("Upgrade, x-hop"),
        );
        request.headers_mut().insert(
            header::HeaderName::from_static("x-hop"),
            header::HeaderValue::from_static("secret-hop"),
        );
        request.headers_mut().insert(
            header::HeaderName::from_static("keep-alive"),
            header::HeaderValue::from_static("timeout=5"),
        );
        request
            .headers_mut()
            .insert(header::TE, header::HeaderValue::from_static("trailers"));
        let (mut client, _) = connect_async(request).await?;
        client
            .send(WebSocketMessage::Text("fallback".into()))
            .await?;
        assert_eq!(
            client.next().await.ok_or("missing fallback echo")??,
            WebSocketMessage::Text("fallback".into())
        );
        client.close(None).await?;

        let private_headers = private_headers_rx.await?;
        assert_eq!(
            http_header_value(&private_headers, "Sec-WebSocket-Extensions"),
            None
        );
        assert_eq!(
            http_header_value(&private_headers, "Sec-WebSocket-Protocol"),
            Some(PRIVATE_WEBSOCKET_SUBPROTOCOL)
        );
        assert_eq!(
            http_header_value(&private_headers, "Connection"),
            Some("Upgrade")
        );
        assert_eq!(
            http_header_value(&private_headers, "Upgrade"),
            Some("websocket")
        );
        assert_eq!(http_header_value(&private_headers, "x-hop"), None);
        assert_eq!(http_header_value(&private_headers, "keep-alive"), None);
        assert_eq!(http_header_value(&private_headers, "TE"), None);
        let fallback_headers = fallback_headers_rx.await?;
        assert_eq!(
            http_header_value(&fallback_headers, "Sec-WebSocket-Extensions"),
            Some("permessage-deflate")
        );
        assert_eq!(
            http_header_value(&fallback_headers, "Sec-WebSocket-Protocol"),
            None
        );
        assert_eq!(
            http_header_value(&fallback_headers, "Connection"),
            Some("upgrade")
        );
        assert_eq!(
            http_header_value(&fallback_headers, "Upgrade"),
            Some("websocket")
        );
        assert_eq!(http_header_value(&fallback_headers, "x-hop"), None);
        assert_eq!(http_header_value(&fallback_headers, "keep-alive"), None);
        assert_eq!(http_header_value(&fallback_headers, "TE"), None);
        assert!(!metrics.snapshot().websocket_zstd_verified);

        proxy.stop().await;
        upstream_task.abort();
        Ok(())
    }

    #[tokio::test]
    async fn accepted_private_protocol_rejects_text_data_frame_with_1002()
    -> Result<(), Box<dyn Error>> {
        let upstream_listener = TcpListener::bind("127.0.0.1:0").await?;
        let upstream_address = upstream_listener.local_addr()?;
        let upstream_task = tokio::spawn(private_invalid_frame_upstream(upstream_listener));
        let metrics = Arc::new(Metrics::default());
        let proxy = start_proxy(ProxyOptions {
            upstream: Url::parse(&format!("http://{upstream_address}/v1"))?,
            compression_enabled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            websocket_enabled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            ai_cove_private_websocket_zstd: true,
            metrics: Arc::clone(&metrics),
            preferred_ports: vec![0],
            max_request_body_bytes: 1024,
        })
        .await?;
        let endpoint = Url::parse(proxy.endpoint())?;
        let request = format!(
            "ws://127.0.0.1:{}/v1/responses",
            endpoint.port().ok_or("missing port")?
        )
        .into_client_request()?;
        let (mut client, _) = connect_async(request).await?;
        client.send(WebSocketMessage::Text("legacy".into())).await?;

        let close = client.next().await.ok_or("missing protocol close")??;
        let WebSocketMessage::Close(Some(frame)) = close else {
            return Err("expected close frame".into());
        };
        assert_eq!(frame.code, CloseCode::Protocol);
        let snapshot = metrics.snapshot();
        assert!(!snapshot.websocket_zstd_verified);
        assert_eq!(snapshot.websocket_failures, 1);
        let event = metrics
            .traffic_snapshot()
            .recent_requests
            .into_iter()
            .last()
            .ok_or("private protocol error traffic event missing")?;
        let event = serde_json::to_value(event)?;
        assert_eq!(
            event.get("status").and_then(serde_json::Value::as_u64),
            Some(1002)
        );
        assert_eq!(
            event.get("result").and_then(serde_json::Value::as_str),
            Some("error")
        );

        proxy.stop().await;
        upstream_task.abort();
        Ok(())
    }

    #[tokio::test]
    async fn private_websocket_reassembles_fragmented_text_before_zstd()
    -> Result<(), Box<dyn Error>> {
        let upstream_listener = TcpListener::bind("127.0.0.1:0").await?;
        let upstream_address = upstream_listener.local_addr()?;
        let (captured_tx, captured_rx) = oneshot::channel();
        let upstream_task = tokio::spawn(private_fragment_upstream(upstream_listener, captured_tx));
        let proxy = start_proxy(ProxyOptions {
            upstream: Url::parse(&format!("http://{upstream_address}/v1"))?,
            compression_enabled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            websocket_enabled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            ai_cove_private_websocket_zstd: true,
            metrics: Arc::new(Metrics::default()),
            preferred_ports: vec![0],
            max_request_body_bytes: 1024,
        })
        .await?;
        let endpoint = Url::parse(proxy.endpoint())?;
        let request = format!(
            "ws://127.0.0.1:{}/v1/responses",
            endpoint.port().ok_or("missing port")?
        )
        .into_client_request()?;
        let (mut client, _) = connect_async(request).await?;

        client
            .send(WebSocketMessage::Frame(Frame::message(
                Bytes::from_static(b"frag"),
                OpCode::Data(Data::Text),
                false,
            )))
            .await?;
        client
            .send(WebSocketMessage::Frame(Frame::message(
                Bytes::from_static(b"ment"),
                OpCode::Data(Data::Continue),
                true,
            )))
            .await?;
        assert_eq!(
            client.next().await.ok_or("missing fragment response")??,
            WebSocketMessage::Text("fragment".into())
        );
        assert_eq!(captured_rx.await?, b"fragment");
        client.close(None).await?;

        proxy.stop().await;
        upstream_task.abort();
        Ok(())
    }

    #[tokio::test]
    async fn private_websocket_keeps_ping_pong_as_control_frames() -> Result<(), Box<dyn Error>> {
        let upstream_listener = TcpListener::bind("127.0.0.1:0").await?;
        let upstream_address = upstream_listener.local_addr()?;
        let (start_tx, start_rx) = oneshot::channel();
        let (captured_tx, captured_rx) = oneshot::channel();
        let upstream_task = tokio::spawn(private_control_upstream(
            upstream_listener,
            start_rx,
            captured_tx,
        ));
        let proxy = start_proxy(ProxyOptions {
            upstream: Url::parse(&format!("http://{upstream_address}/v1"))?,
            compression_enabled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            websocket_enabled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            ai_cove_private_websocket_zstd: true,
            metrics: Arc::new(Metrics::default()),
            preferred_ports: vec![0],
            max_request_body_bytes: 1024,
        })
        .await?;
        let endpoint = Url::parse(proxy.endpoint())?;
        let request = format!(
            "ws://127.0.0.1:{}/v1/responses",
            endpoint.port().ok_or("missing port")?
        )
        .into_client_request()?;
        let (mut client, _) = connect_async(request).await?;
        let probe = Bytes::from_static(b"client-ping");

        client.send(WebSocketMessage::Ping(probe.clone())).await?;
        let reply = tokio::time::timeout(Duration::from_secs(1), client.next())
            .await?
            .ok_or("missing pong")??;
        assert_eq!(reply, WebSocketMessage::Pong(probe));
        client.send(WebSocketMessage::Text("legacy".into())).await?;
        start_tx.send(()).map_err(|()| "upstream start failed")?;
        assert_eq!(captured_rx.await?, Bytes::from_static(b"upstream-ping"));
        let unexpected = tokio::time::timeout(Duration::from_millis(250), client.next()).await;
        assert!(
            unexpected.is_err(),
            "control frames must not cross websocket legs: {unexpected:?}"
        );
        client.close(None).await?;

        proxy.stop().await;
        upstream_task.abort();
        Ok(())
    }

    #[tokio::test]
    async fn websocket_upgrade_preserves_extensions_bytes_and_close() -> Result<(), Box<dyn Error>>
    {
        let upstream_listener = TcpListener::bind("127.0.0.1:0").await?;
        let upstream_address = upstream_listener.local_addr()?;
        let (captured_tx, captured_rx) = oneshot::channel();
        let upstream_task = tokio::spawn(async move {
            let Ok((mut stream, _)) = upstream_listener.accept().await else {
                return;
            };
            let Ok(head) = read_http_head(&mut stream).await else {
                return;
            };
            let _ = captured_tx.send(String::from_utf8_lossy(&head).into_owned());
            let response = b"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Accept: test-only\r\nSec-WebSocket-Extensions: permessage-deflate\r\n\r\n";
            if stream.write_all(response).await.is_err() {
                return;
            }
            let mut payload = [0_u8; 4];
            if stream.read_exact(&mut payload).await.is_ok() {
                let _ = stream.write_all(&payload).await;
            }
        });
        let metrics = Arc::new(Metrics::default());
        let proxy = start_proxy(ProxyOptions {
            upstream: Url::parse(&format!("http://{upstream_address}/v1"))?,
            compression_enabled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            websocket_enabled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            ai_cove_private_websocket_zstd: false,
            metrics: Arc::clone(&metrics),
            preferred_ports: vec![0],
            max_request_body_bytes: 1024,
        })
        .await?;
        let endpoint = Url::parse(proxy.endpoint())?;
        let mut client =
            TcpStream::connect(("127.0.0.1", endpoint.port().ok_or("missing port")?)).await?;
        client
            .write_all(
                b"GET /v1/responses HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: keep-alive, Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Key: test-only\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Extensions: permessage-deflate\r\nAuthorization: Bearer test-only\r\n\r\n",
            )
            .await?;

        let response = String::from_utf8(read_http_head(&mut client).await?)?;
        assert!(response.starts_with("HTTP/1.1 101"));
        assert!(
            response
                .to_ascii_lowercase()
                .contains("sec-websocket-extensions: permessage-deflate")
        );
        let payload = [0x82, 0x02, b'o', b'k'];
        client.write_all(&payload).await?;
        client.shutdown().await?;
        let mut echoed = [0_u8; 4];
        client.read_exact(&mut echoed).await?;
        assert_eq!(echoed, payload);
        assert_eq!(client.read(&mut [0_u8; 1]).await?, 0);

        let upstream_head = captured_rx.await?;
        assert!(upstream_head.starts_with("GET /v1/responses HTTP/1.1"));
        assert!(
            upstream_head
                .to_ascii_lowercase()
                .contains("sec-websocket-extensions: permessage-deflate")
        );
        assert!(
            upstream_head
                .to_ascii_lowercase()
                .contains("authorization: bearer test-only")
        );
        let snapshot = metrics.snapshot();
        assert!(snapshot.websocket_verified);
        assert_eq!(snapshot.websocket_handshakes, 1);
        assert_eq!(snapshot.http_fallbacks, 0);
        tokio::time::timeout(Duration::from_secs(1), metrics.traffic_recorded.notified()).await?;
        let event = metrics
            .traffic_snapshot()
            .recent_requests
            .into_iter()
            .next()
            .ok_or("standard websocket traffic event missing")?;
        let event = serde_json::to_value(event)?;
        assert_eq!(
            event.get("status").and_then(serde_json::Value::as_u64),
            Some(101)
        );
        assert_eq!(
            event.get("rawBytes").and_then(serde_json::Value::as_u64),
            Some(4)
        );
        assert_eq!(
            event.get("sentBytes").and_then(serde_json::Value::as_u64),
            Some(4)
        );

        proxy.stop().await;
        upstream_task.abort();
        Ok(())
    }

    #[tokio::test]
    async fn websocket_failure_does_not_mark_unrelated_http_as_fallback()
    -> Result<(), Box<dyn Error>> {
        let captured = CapturedRequest::default();
        let upstream_listener = TcpListener::bind("127.0.0.1:0").await?;
        let upstream_address = upstream_listener.local_addr()?;
        let upstream_app = Router::new()
            .route("/v1/responses", post(upstream))
            .with_state(Arc::clone(&captured));
        let upstream_task = tokio::spawn(async move {
            let _ = axum::serve(upstream_listener, upstream_app).await;
        });
        let metrics = Arc::new(Metrics::default());
        let proxy = start_proxy(ProxyOptions {
            upstream: Url::parse(&format!("http://{upstream_address}/v1"))?,
            compression_enabled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            websocket_enabled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            ai_cove_private_websocket_zstd: false,
            metrics: Arc::clone(&metrics),
            preferred_ports: vec![0],
            max_request_body_bytes: 1024 * 1024,
        })
        .await?;
        let endpoint = Url::parse(proxy.endpoint())?;
        let mut client =
            TcpStream::connect(("127.0.0.1", endpoint.port().ok_or("missing port")?)).await?;
        client
            .write_all(
                b"GET /v1/responses HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Key: test-only\r\nSec-WebSocket-Version: 13\r\n\r\n",
            )
            .await?;
        let response = String::from_utf8(read_http_head(&mut client).await?)?;
        assert!(response.starts_with("HTTP/1.1 503"));

        let input = serde_json::json!({ "input": "repeat ".repeat(2000) }).to_string();
        let fallback = reqwest::Client::new()
            .post(format!("{}/responses", proxy.endpoint()))
            .header("content-type", "application/json")
            .body(input)
            .send()
            .await?;
        assert_eq!(fallback.status(), StatusCode::CREATED);
        let (headers, _) = captured.lock().await.take().ok_or("missing fallback")?;
        assert_eq!(
            headers
                .get("content-encoding")
                .and_then(|value| value.to_str().ok()),
            Some("zstd")
        );
        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.http_fallbacks, 0);
        assert!(snapshot.compression_verified);
        assert_eq!(snapshot.websocket_handshakes, 0);

        proxy.stop().await;
        upstream_task.abort();
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires AI_COVE_API_KEY and the live AI Cove endpoint"]
    async fn live_ai_cove_websocket_handshake_passes_through_turbo() -> Result<(), Box<dyn Error>> {
        let api_key = std::env::var("AI_COVE_API_KEY")?;
        let metrics = Arc::new(Metrics::default());
        let proxy = start_proxy(ProxyOptions {
            upstream: Url::parse("https://api.ai-cove.com/v1")?,
            compression_enabled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            websocket_enabled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            ai_cove_private_websocket_zstd: true,
            metrics: Arc::clone(&metrics),
            preferred_ports: vec![0],
            max_request_body_bytes: 1024,
        })
        .await?;
        let endpoint = Url::parse(proxy.endpoint())?;
        let mut client =
            TcpStream::connect(("127.0.0.1", endpoint.port().ok_or("missing port")?)).await?;
        let request = format!(
            "GET /v1/responses HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Extensions: permessage-deflate\r\nAuthorization: Bearer {api_key}\r\n\r\n"
        );
        client.write_all(request.as_bytes()).await?;

        let response = String::from_utf8(read_http_head(&mut client).await?)?;
        assert!(response.starts_with("HTTP/1.1 101"));
        client.shutdown().await?;
        for _ in 0..20 {
            if metrics.snapshot().websocket_handshakes == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let snapshot = metrics.snapshot();
        assert!(snapshot.websocket_verified);
        assert_eq!(snapshot.websocket_handshakes, 1);

        proxy.stop().await;
        Ok(())
    }

    #[tokio::test]
    #[ignore = "requires codex, AI_COVE_API_KEY, and the live AI Cove endpoint"]
    async fn live_codex_request_uses_turbo_websocket() -> Result<(), Box<dyn Error>> {
        let api_key = std::env::var("AI_COVE_API_KEY")?;
        let metrics = Arc::new(Metrics::default());
        let proxy = start_proxy(ProxyOptions {
            upstream: Url::parse("https://api.ai-cove.com/v1")?,
            compression_enabled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            websocket_enabled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            ai_cove_private_websocket_zstd: true,
            metrics: Arc::clone(&metrics),
            preferred_ports: vec![0],
            max_request_body_bytes: 128 * 1024 * 1024,
        })
        .await?;
        let codex_home = tempfile::tempdir()?;
        std::fs::write(
            codex_home.path().join("config.toml"),
            format!(
                r#"model = "gpt-5.6-luna"
model_provider = "turbo-test"
approval_policy = "never"
sandbox_mode = "read-only"

[model_providers.turbo-test]
name = "Turbo production test"
base_url = "{}"
env_key = "AI_COVE_API_KEY"
wire_api = "responses"
requires_openai_auth = false
supports_websockets = true
"#,
                proxy.endpoint()
            ),
        )?;
        let mut child = Command::new("codex")
            .args([
                "exec",
                "--skip-git-repo-check",
                "--ephemeral",
                "--sandbox",
                "read-only",
                "请执行一次只读 shell 工具 pwd，读取工具结果后只回复 OK，不要再调用工具。",
            ])
            .env("CODEX_HOME", codex_home.path())
            .env("AI_COVE_API_KEY", api_key)
            .current_dir(codex_home.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let mut exit_status = None;
        for _ in 0..900 {
            if let Some(status) = child.try_wait()? {
                exit_status = Some(status);
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        if exit_status.is_none() {
            child.kill()?;
        }
        assert!(exit_status.is_some_and(|status| status.success()));
        for _ in 0..50 {
            if metrics.snapshot().websocket_active == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let snapshot = metrics.snapshot();
        let reconnects = snapshot.websocket_handshakes.saturating_sub(1);
        let messages_per_connection =
            (snapshot.websocket_handshakes == 1).then_some(snapshot.websocket_messages);
        eprintln!(
            "real Codex WebSocket lifecycle evidence: handshakes={}, messages={}, messages_per_connection={messages_per_connection:?}, reconnects={}, failures={}",
            snapshot.websocket_handshakes,
            snapshot.websocket_messages,
            reconnects,
            snapshot.websocket_failures,
        );
        assert!(snapshot.websocket_handshakes > 0);
        assert!(snapshot.websocket_messages > 0);
        if let Some(messages_per_connection) = messages_per_connection {
            assert!(
                messages_per_connection >= 2,
                "real Codex session did not produce a multi-message connection: handshakes={}, messages={}, reconnects={reconnects}",
                snapshot.websocket_handshakes,
                snapshot.websocket_messages,
            );
        }
        assert!(snapshot.websocket_zstd_verified);
        assert!(snapshot.websocket_sent_bytes < snapshot.websocket_raw_bytes);

        proxy.stop().await;
        Ok(())
    }
}
