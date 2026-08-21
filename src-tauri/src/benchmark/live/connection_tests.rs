use std::{
    collections::HashSet,
    io,
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use axum::{
    Router,
    extract::{ConnectInfo, State},
    http::StatusCode,
    routing::post,
};
use tokio::sync::OnceCell;
use url::Url;

use super::{
    BenchmarkPath, BenchmarkResult, LiveContext, PayloadSet, benchmark_error, require_compression,
    sample_context_error,
};
use crate::benchmark::{
    BenchmarkCase, BenchmarkSettings, RoundTransport, Sample, settings::WorkloadSource,
};

type Peers = Arc<Mutex<HashSet<SocketAddr>>>;

async fn complete_response(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    State(peers): State<Peers>,
) -> &'static str {
    if let Ok(mut peers) = peers.lock() {
        peers.insert(peer);
    }
    "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp-test\"}}\n\n"
}

async fn transient_then_complete(
    State(attempts): State<Arc<AtomicUsize>>,
) -> (StatusCode, &'static str) {
    if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
        return (StatusCode::SERVICE_UNAVAILABLE, "temporary overload");
    }
    (
        StatusCode::OK,
        "data: {\"type\":\"response.completed\"}\n\n",
    )
}

#[tokio::test]
async fn reuses_persistent_turbo_http_connection_between_samples() -> BenchmarkResult<()> {
    // Given
    let peers = Peers::default();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let address = listener.local_addr()?;
    let server_peers = Arc::clone(&peers);
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route("/v1/responses", post(complete_response))
                .with_state(server_peers)
                .into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
    });
    let upstream = Url::parse(&format!("http://{address}/v1")).map_err(benchmark_error)?;
    let url = format!("http://{address}/v1/responses");
    let settings = BenchmarkSettings {
        upstream: format!("http://{address}"),
        model: "test-model".to_owned(),
        prompt: "test-prompt".to_owned(),
        workload_source: WorkloadSource::BuiltIn,
        runs: 3,
        warmups: 0,
        timeout: Duration::from_secs(1),
    };
    let context = LiveContext {
        settings: &settings,
        authorization: "test-key",
        upstream: &upstream,
        direct_url: &url,
        http_proxy: OnceCell::new(),
        hybrid_proxy: OnceCell::new(),
    };
    let http = vec![
        "{\"input\":\"one\"}".to_owned(),
        "{\"input\":\"two\"}".to_owned(),
    ];
    let payloads = PayloadSet {
        http: &http,
        websocket: &[],
    };

    // When
    let first = context
        .collect_sample(BenchmarkPath::Http, &payloads)
        .await?;
    let second = context
        .collect_sample(BenchmarkPath::Http, &payloads)
        .await?;

    // Then
    let peer_count = peers
        .lock()
        .map_err(|_| io::Error::other("peer set lock poisoned"))?
        .len();
    assert_eq!(peer_count, 1);
    assert_eq!(first.warm_round_e2e.len(), 1);
    assert_eq!(second.warm_round_e2e.len(), 1);
    context.stop().await;
    server.abort();
    Ok(())
}

#[tokio::test]
async fn records_one_retry_for_a_transient_upstream_sample() -> BenchmarkResult<()> {
    let attempts = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let address = listener.local_addr()?;
    let server_attempts = Arc::clone(&attempts);
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route("/v1/responses", post(transient_then_complete))
                .with_state(server_attempts),
        )
        .await
    });
    let upstream = Url::parse(&format!("http://{address}/v1")).map_err(benchmark_error)?;
    let url = format!("http://{address}/v1/responses");
    let settings = BenchmarkSettings {
        upstream: format!("http://{address}"),
        model: "test-model".to_owned(),
        prompt: "test-prompt".to_owned(),
        workload_source: WorkloadSource::BuiltIn,
        runs: 4,
        warmups: 0,
        timeout: Duration::from_secs(1),
    };
    let context = LiveContext {
        settings: &settings,
        authorization: "test-key",
        upstream: &upstream,
        direct_url: &url,
        http_proxy: OnceCell::new(),
        hybrid_proxy: OnceCell::new(),
    };
    let http = vec!["{\"input\":\"one\"}".to_owned()];
    let payloads = PayloadSet {
        http: &http,
        websocket: &[],
    };

    let sample = context
        .collect_sample(BenchmarkPath::Direct, &payloads)
        .await?;

    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert_eq!(sample.retries, 1);
    assert_eq!(sample.logical_requests, 1);
    assert_eq!(sample.http_requests, 1);
    context.stop().await;
    server.abort();
    Ok(())
}

#[test]
fn rotates_four_path_order_to_balance_time_drift() {
    assert_eq!(
        super::rotated_paths(0),
        [
            BenchmarkPath::Direct,
            BenchmarkPath::Http,
            BenchmarkPath::WebSocket,
            BenchmarkPath::Hybrid,
        ]
    );
    assert_eq!(
        super::rotated_paths(1),
        [
            BenchmarkPath::Http,
            BenchmarkPath::WebSocket,
            BenchmarkPath::Hybrid,
            BenchmarkPath::Direct,
        ]
    );
    assert_eq!(
        super::rotated_paths(2),
        [
            BenchmarkPath::WebSocket,
            BenchmarkPath::Hybrid,
            BenchmarkPath::Direct,
            BenchmarkPath::Http,
        ]
    );
    assert_eq!(
        super::rotated_paths(3),
        [
            BenchmarkPath::Hybrid,
            BenchmarkPath::Direct,
            BenchmarkPath::Http,
            BenchmarkPath::WebSocket,
        ]
    );
    assert_eq!(super::rotated_paths(4), super::rotated_paths(0));
}

#[test]
fn compression_gate_ignores_recovered_samples() -> BenchmarkResult<()> {
    let sample = |encoded_bytes, retries| Sample {
        e2e: Duration::ZERO,
        setup: Duration::ZERO,
        raw_bytes: 10,
        encoded_bytes,
        logical_requests: 1,
        application_messages: 1,
        http_requests: 1,
        websocket_messages: 0,
        response_events: 1,
        websocket_handshakes: 0,
        round_e2e: vec![Duration::ZERO],
        first_events: vec![Duration::ZERO],
        warm_round_e2e: vec![],
        connection_lifetime: None,
        websocket_reconnects: 0,
        messages_per_connection: None,
        retries,
        round_transports: vec![RoundTransport::Http],
        compression_metrics: None,
    };
    let case = BenchmarkCase {
        scenario: "long-input",
        path: "Turbo HTTP + zstd",
        samples: vec![sample(10, 1), sample(5, 0)],
    };

    require_compression(&case, true)
}

#[test]
fn compression_gate_rejects_all_recovered_samples() {
    let case = BenchmarkCase {
        scenario: "long-input",
        path: "Turbo HTTP + zstd",
        samples: vec![Sample {
            e2e: Duration::ZERO,
            setup: Duration::ZERO,
            raw_bytes: 10,
            encoded_bytes: 5,
            logical_requests: 1,
            application_messages: 1,
            http_requests: 1,
            websocket_messages: 0,
            response_events: 1,
            websocket_handshakes: 0,
            round_e2e: vec![Duration::ZERO],
            first_events: vec![Duration::ZERO],
            warm_round_e2e: vec![],
            connection_lifetime: None,
            websocket_reconnects: 0,
            messages_per_connection: None,
            retries: 1,
            round_transports: vec![RoundTransport::Http],
            compression_metrics: None,
        }],
    };

    let error = require_compression(&case, true)
        .expect_err("all-recovered compression case must fail")
        .to_string();
    assert!(error.contains("没有无重试的有效样本"));
}

#[test]
fn reports_scenario_path_and_iteration_for_sample_failures() {
    let cause = io::Error::other("HTTP 524");
    let error = sample_context_error("单轮长上下文", BenchmarkPath::Http, 7, &cause);
    let message = error.to_string();

    assert!(message.contains("scenario=单轮长上下文"));
    assert!(message.contains("path=Turbo HTTP + 自适应 zstd"));
    assert!(message.contains("iteration=7"));
    assert!(message.contains("HTTP 524"));
}
