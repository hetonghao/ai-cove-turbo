use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    extract::{Request as AxumRequest, State},
    http::{Response, StatusCode},
    routing::get,
};
use futures_util::StreamExt;
use hyper::upgrade::OnUpgrade;
use hyper_rustls::ConfigBuilderExt;
use hyper_util::rt::TokioIo;
use tokio::sync::oneshot;
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::protocol::{Role, WebSocketConfig},
};
use url::Url;

use super::{
    Metrics, PRIVATE_TLS_SESSION_CACHE_SIZE, ProxyError, ProxyHandle, json_error,
    private_websocket, resolve_target,
};

#[cfg(test)]
#[path = "private_websocket_benchmark_test.rs"]
mod tests;

#[derive(Clone)]
struct StateData {
    upstream: Url,
    metrics: Arc<Metrics>,
    tls_config: private_websocket::PrivateTlsConfig,
}

pub(crate) async fn start(upstream: Url, metrics: Arc<Metrics>) -> Result<ProxyHandle, ProxyError> {
    let listener = super::bind_preferred(&[0]).await?;
    let address = listener.local_addr().map_err(ProxyError::Bind)?;
    let endpoint = format!("http://{address}/v1");
    let tls_config = private_tls_config()?;
    let hybrid_pool = super::hybrid_pool::HybridPool::new(tls_config.clone(), Arc::clone(&metrics));
    let app = Router::new()
        .route("/healthz", get(super::health))
        .fallback(handle_upgrade)
        .with_state(StateData {
            upstream,
            metrics,
            tls_config,
        });
    let (shutdown, receiver) = oneshot::channel();
    let task = tokio::spawn(async move {
        let server = axum::serve(listener, app).with_graceful_shutdown(async move {
            let _ = receiver.await;
        });
        let _ = server.await;
    });
    if let Err(error) = wait_for_health(address).await {
        let _ = shutdown.send(());
        let _ = task.await;
        return Err(error);
    }
    Ok(ProxyHandle {
        endpoint,
        hybrid_pool,
        shutdown: Some(shutdown),
        task,
    })
}

async fn wait_for_health(address: std::net::SocketAddr) -> Result<(), ProxyError> {
    let health_url = format!("http://{address}/healthz");
    let client = reqwest::Client::new();
    for _ in 0..20 {
        match client.get(&health_url).send().await {
            Ok(response) if response.status().is_success() => return Ok(()),
            _ => tokio::time::sleep(std::time::Duration::from_millis(10)).await,
        }
    }
    Err(ProxyError::HealthCheck)
}

fn private_tls_config() -> Result<private_websocket::PrivateTlsConfig, ProxyError> {
    let mut config = rustls::ClientConfig::builder()
        .with_native_roots()
        .map_err(|error| ProxyError::WebSocketClient(error.to_string()))?
        .with_no_client_auth();
    config.resumption =
        rustls::client::Resumption::in_memory_sessions(PRIVATE_TLS_SESSION_CACHE_SIZE);
    Ok(private_websocket::PrivateTlsConfig::new(Arc::new(config)))
}

async fn handle_upgrade(
    State(state): State<StateData>,
    mut request: AxumRequest,
) -> Response<Body> {
    let headers = request.headers().clone();
    let path = request.uri().path().to_owned();
    let client_upgrade = hyper::upgrade::on(&mut request);
    let Ok(response) = private_websocket::client_upgrade_response(&headers) else {
        return json_error(StatusCode::BAD_REQUEST, "invalid websocket handshake");
    };
    let target = resolve_target(&state.upstream, request.uri());
    let Some((upstream, _server_trace)) =
        private_websocket::connect_private(&target, &headers, &state.tls_config)
            .await
            .ok()
    else {
        state
            .metrics
            .record_websocket_error(&path, StatusCode::BAD_GATEWAY.as_u16());
        return json_error(StatusCode::BAD_GATEWAY, "private websocket upstream failed");
    };
    tokio::spawn(relay_after_upgrade(
        client_upgrade,
        upstream,
        state.metrics,
        path,
    ));
    response
}

async fn relay_after_upgrade(
    client_upgrade: OnUpgrade,
    mut upstream: private_websocket::PrivateUpstream,
    metrics: Arc<Metrics>,
    path: String,
) {
    let Ok(upgraded) = client_upgrade.await else {
        metrics.record_websocket_error(&path, StatusCode::INTERNAL_SERVER_ERROR.as_u16());
        let _ = upstream.close(None).await;
        return;
    };
    let mut client = WebSocketStream::from_raw_socket(
        TokioIo::new(upgraded),
        Role::Server,
        Some(WebSocketConfig::default()),
    )
    .await;
    metrics.record_websocket_connected();
    let Some(message) = client.next().await else {
        let _ = upstream.close(None).await;
        metrics.record_websocket_closed();
        return;
    };
    let Ok(message) = message else {
        metrics.record_websocket_error(&path, 1002);
        let _ = upstream.close(None).await;
        metrics.record_websocket_closed();
        return;
    };
    private_websocket::relay_private_from_message(
        &mut client,
        &mut upstream,
        message,
        Arc::clone(&metrics),
        path,
    )
    .await;
    metrics.record_websocket_closed();
}
