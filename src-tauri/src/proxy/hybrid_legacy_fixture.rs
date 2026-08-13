use std::{io, sync::Arc, time::Duration};

use axum::{
    Router,
    body::Body,
    extract::{Request, State},
    http::{Response, StatusCode, header},
};
use futures_util::{SinkExt, StreamExt};
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{
        Message,
        handshake::derive_accept_key,
        protocol::{CloseFrame, Role, frame::coding::CloseCode},
    },
};
use url::Url;

use crate::proxy::{Metrics, ProxyHandle, ProxyOptions, start_proxy};

pub(super) struct StandardFixture {
    upstream: Url,
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl StandardFixture {
    pub(super) async fn start() -> io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let upstream = Url::parse(&format!("http://{}/v1", listener.local_addr()?))
            .map_err(io::Error::other)?;
        let (shutdown, receiver) = oneshot::channel();
        let app = Router::new().fallback(upstream_request).with_state(());
        let task = tokio::spawn(async move {
            let server = axum::serve(listener, app).with_graceful_shutdown(async move {
                let _ = receiver.await;
            });
            let _ = server.await;
        });
        Ok(Self {
            upstream,
            shutdown: Some(shutdown),
            task,
        })
    }

    pub(super) async fn stop(mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        let _ = self.task.await;
    }
}

pub(super) async fn start_standard_proxy(
    fixture: &StandardFixture,
) -> io::Result<(ProxyHandle, Arc<Metrics>)> {
    let metrics = Arc::new(Metrics::default());
    let proxy = start_proxy(ProxyOptions {
        upstream: fixture.upstream.clone(),
        compression_enabled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        websocket_enabled: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        ai_cove_private_websocket_zstd: true,
        metrics: Arc::clone(&metrics),
        preferred_ports: vec![0],
        max_request_body_bytes: 1024 * 1024,
    })
    .await
    .map_err(io::Error::other)?;
    Ok((proxy, metrics))
}

pub(super) async fn wait_for_traffic(metrics: &Metrics) -> io::Result<()> {
    tokio::time::timeout(Duration::from_secs(1), metrics.traffic_recorded.notified())
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "traffic outcome did not arrive"))?;
    Ok(())
}

async fn upstream_request(State(()): State<()>, mut request: Request) -> Response<Body> {
    let private = request
        .headers()
        .get(header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == "ai-cove-zstd.v1");
    if private {
        let mut response = Response::new(Body::empty());
        *response.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
        return response;
    }
    upgrade_standard(&mut request)
}

fn upgrade_standard(request: &mut Request) -> Response<Body> {
    let Some(key) = request.headers().get(header::SEC_WEBSOCKET_KEY) else {
        return Response::new(Body::empty());
    };
    let accept = derive_accept_key(key.as_bytes());
    let Ok(accept) = header::HeaderValue::from_str(&accept) else {
        return Response::new(Body::empty());
    };
    let upgrade = hyper::upgrade::on(request);
    tokio::spawn(async move {
        let Ok(upgraded) = upgrade.await else {
            return;
        };
        let websocket = WebSocketStream::from_raw_socket(
            hyper_util::rt::TokioIo::new(upgraded),
            Role::Server,
            None,
        )
        .await;
        run_standard(websocket).await;
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
    response
}

async fn run_standard<S>(mut websocket: WebSocketStream<S>)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    while let Some(Ok(message)) = websocket.next().await {
        let payload = match message {
            Message::Text(text) => text.as_bytes().to_vec(),
            Message::Binary(payload) => payload.to_vec(),
            Message::Ping(payload) => {
                let _ = websocket.send(Message::Pong(payload)).await;
                continue;
            }
            Message::Pong(_) => continue,
            Message::Close(frame) => {
                let _ = websocket.send(Message::Close(frame)).await;
                return;
            }
            Message::Frame(_) => return,
        };
        let Ok(request) = serde_json::from_slice::<serde_json::Value>(&payload) else {
            continue;
        };
        if request.get("type").and_then(serde_json::Value::as_str) != Some("response.create") {
            continue;
        }
        match request.get("model").and_then(serde_json::Value::as_str) {
            Some("close-active") => {
                let _ = websocket
                    .send(Message::Close(Some(CloseFrame {
                        code: CloseCode::Error,
                        reason: "upstream closed before terminal".into(),
                    })))
                    .await;
                return;
            }
            Some("failed-terminal") => {
                if websocket
                    .send(Message::Text(
                        r#"{"type":"response.failed","status":500}"#.into(),
                    ))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            Some(_) | None => {
                if websocket
                    .send(Message::Text(r#"{"type":"response.completed"}"#.into()))
                    .await
                    .is_err()
                {
                    return;
                }
            }
        }
    }
}
