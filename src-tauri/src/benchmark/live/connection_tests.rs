use std::{
    collections::HashSet,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Router,
    extract::{ConnectInfo, State},
    routing::post,
};
use url::Url;

use super::{BenchmarkPath, LiveContext, PayloadSet};
use crate::{
    benchmark::{BenchmarkSettings, settings::WorkloadSource},
    proxy::Metrics,
};

type Peers = Arc<Mutex<HashSet<SocketAddr>>>;

async fn complete_response(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    State(peers): State<Peers>,
) -> &'static str {
    peers
        .lock()
        .expect("peer set lock must be healthy")
        .insert(peer);
    "data: {\"type\":\"response.completed\"}\n\n"
}

#[tokio::test]
async fn reuses_http_connection_within_sample_but_not_between_samples()
-> Result<(), Box<dyn std::error::Error>> {
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
    let upstream = Url::parse(&format!("http://{address}/v1"))?;
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
    let metrics = Metrics::default();
    let context = LiveContext {
        settings: &settings,
        authorization: "test-key",
        upstream: &upstream,
        direct_url: &url,
        websocket_url: "ws://127.0.0.1/unused",
        websocket_metrics: &metrics,
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
    assert_eq!(
        peers.lock().expect("peer set lock must be healthy").len(),
        2
    );
    assert_eq!(first.warm_round_e2e.len(), 1);
    assert_eq!(second.warm_round_e2e.len(), 1);
    server.abort();
    Ok(())
}
