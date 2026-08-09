use std::{io, sync::Arc, time::Duration};

use axum::{
    Router,
    body::Body,
    extract::{Request, State},
    http::{Response, StatusCode, header},
};
use futures_util::StreamExt;
use tokio::{
    net::TcpListener,
    sync::{Notify, oneshot},
    task::JoinHandle,
};
use tokio_tungstenite::{connect_async, tungstenite::handshake::derive_accept_key};
use url::Url;

use super::super::private_websocket::PRIVATE_WEBSOCKET_SUBPROTOCOL;
use crate::proxy::{Metrics, private_websocket_benchmark::start};

struct UpstreamFixture {
    url: Url,
    started: Arc<Notify>,
    release: Arc<Notify>,
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

#[derive(Clone)]
struct Gate {
    started: Arc<Notify>,
    release: Arc<Notify>,
}

impl UpstreamFixture {
    async fn start() -> io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let address = listener.local_addr()?;
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let gate = Gate {
            started: Arc::clone(&started),
            release: Arc::clone(&release),
        };
        let (shutdown, receiver) = oneshot::channel();
        let app = Router::new().fallback(private_upgrade).with_state(gate);
        let task = tokio::spawn(async move {
            let server = axum::serve(listener, app).with_graceful_shutdown(async move {
                let _ = receiver.await;
            });
            let _ = server.await;
        });
        Ok(Self {
            url: Url::parse(&format!("http://{address}/v1")).map_err(io::Error::other)?,
            started,
            release,
            shutdown: Some(shutdown),
            task,
        })
    }

    async fn stop(mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        let _ = self.task.await;
    }
}

async fn private_upgrade(State(gate): State<Gate>, mut request: Request) -> Response<Body> {
    let private = request
        .headers()
        .get(header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|value| value.to_str().ok())
        == Some(PRIVATE_WEBSOCKET_SUBPROTOCOL);
    if !private {
        let mut response = Response::new(Body::empty());
        *response.status_mut() = StatusCode::BAD_REQUEST;
        return response;
    }
    gate.started.notify_one();
    gate.release.notified().await;
    let Some(key) = request.headers().get(header::SEC_WEBSOCKET_KEY) else {
        return Response::new(Body::empty());
    };
    let accept = derive_accept_key(key.as_bytes());
    let Ok(accept) = header::HeaderValue::from_str(&accept) else {
        return Response::new(Body::empty());
    };
    let upgrade = hyper::upgrade::on(&mut request);
    tokio::spawn(async move {
        let Ok(upgraded) = upgrade.await else {
            return;
        };
        let mut stream = tokio_tungstenite::WebSocketStream::from_raw_socket(
            hyper_util::rt::TokioIo::new(upgraded),
            tokio_tungstenite::tungstenite::protocol::Role::Server,
            None,
        )
        .await;
        let _ = stream.next().await;
    });
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
    response.headers_mut().insert(
        header::CONNECTION,
        header::HeaderValue::from_static("upgrade"),
    );
    response.headers_mut().insert(
        header::UPGRADE,
        header::HeaderValue::from_static("websocket"),
    );
    response
        .headers_mut()
        .insert(header::SEC_WEBSOCKET_ACCEPT, accept);
    response.headers_mut().insert(
        header::SEC_WEBSOCKET_PROTOCOL,
        header::HeaderValue::from_static(PRIVATE_WEBSOCKET_SUBPROTOCOL),
    );
    response
}

#[tokio::test]
async fn local_101_waits_for_public_private_handshake() -> io::Result<()> {
    let upstream = UpstreamFixture::start().await?;
    let metrics = Arc::new(Metrics::default());
    let proxy = start(upstream.url.clone(), Arc::clone(&metrics))
        .await
        .map_err(io::Error::other)?;
    let mut local_url = Url::parse(proxy.endpoint()).map_err(io::Error::other)?;
    local_url
        .set_scheme("ws")
        .map_err(|()| io::Error::other("local websocket scheme is invalid"))?;
    local_url.set_path("/v1/responses");
    let mut connect = tokio::spawn(connect_async(local_url.to_string()));
    upstream.started.notified().await;
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut connect)
            .await
            .is_err()
    );
    upstream.release.notify_one();
    let (_, response) = tokio::time::timeout(Duration::from_secs(1), &mut connect)
        .await
        .map_err(io::Error::other)?
        .map_err(io::Error::other)?
        .map_err(io::Error::other)?;
    assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
    proxy.stop().await;
    upstream.stop().await;
    Ok(())
}
