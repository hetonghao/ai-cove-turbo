use axum::{
    extract::Request as AxumRequest,
    http::{HeaderMap, StatusCode, Uri},
};
use futures_util::StreamExt;
use hyper::{upgrade::OnUpgrade, upgrade::Upgraded};
use hyper_util::rt::TokioIo;
use tokio::{sync::mpsc, task::JoinHandle};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{
        Error as WebSocketError, Message,
        protocol::{Role, WebSocketConfig},
    },
};
use url::Url;

use super::{ProxyState, private_websocket};

#[path = "hybrid_active.rs"]
mod active;
#[path = "hybrid_common.rs"]
mod common;
#[path = "hybrid_flow.rs"]
mod flow;
#[path = "hybrid_http.rs"]
mod http;
#[cfg(test)]
#[path = "hybrid_integration_server.rs"]
mod integration_server;
#[cfg(test)]
#[path = "hybrid_integration_state.rs"]
mod integration_state;
#[cfg(test)]
#[path = "hybrid_integration_tests.rs"]
mod integration_tests;
#[path = "hybrid_legacy.rs"]
mod legacy;
#[path = "hybrid_sse.rs"]
mod sse;
#[cfg(test)]
#[path = "hybrid_tests.rs"]
mod tests;
#[path = "hybrid_websocket.rs"]
mod websocket;

type ClientWebSocket = WebSocketStream<TokioIo<Upgraded>>;
type PrivateWebSocket = private_websocket::PrivateUpstream;

const WEBSOCKET_MESSAGE_LIMIT: usize = 128 * 1024 * 1024;

pub(super) fn spawn(state: ProxyState, request: &mut AxumRequest, target: Url, path: String) {
    let client_upgrade = hyper::upgrade::on(&mut *request);
    let client_headers = request.headers().clone();
    let request_uri = request.uri().clone();
    tokio::spawn(async move {
        run_after_upgrade(
            state,
            client_upgrade,
            client_headers,
            request_uri,
            target,
            path,
        )
        .await;
    });
}

async fn run_after_upgrade(
    state: ProxyState,
    client_upgrade: OnUpgrade,
    client_headers: HeaderMap,
    request_uri: Uri,
    target: Url,
    path: String,
) {
    let Ok(upgraded) = client_upgrade.await else {
        state
            .metrics
            .record_websocket_error(&path, StatusCode::INTERNAL_SERVER_ERROR.as_u16());
        return;
    };
    let config = WebSocketConfig::default()
        .max_message_size(Some(WEBSOCKET_MESSAGE_LIMIT))
        .max_frame_size(Some(WEBSOCKET_MESSAGE_LIMIT));
    let client =
        WebSocketStream::from_raw_socket(TokioIo::new(upgraded), Role::Server, Some(config)).await;
    let mut session = Session::new(state, client_headers, request_uri, target, path);
    run_session(&mut session, client).await;
}

struct Session {
    state: ProxyState,
    client_headers: HeaderMap,
    request_uri: Uri,
    target: Url,
    path: String,
    ready: Option<PrivateWebSocket>,
    prewarm_attempted: bool,
    prewarm_task: Option<JoinHandle<()>>,
    prewarm_rx: mpsc::Receiver<Option<PrivateWebSocket>>,
    prewarm_tx: mpsc::Sender<Option<PrivateWebSocket>>,
    first_response_pending: bool,
    force_http: bool,
}

impl Session {
    fn new(
        state: ProxyState,
        client_headers: HeaderMap,
        request_uri: Uri,
        target: Url,
        path: String,
    ) -> Self {
        let (prewarm_tx, prewarm_rx) = mpsc::channel(1);
        Self {
            state,
            client_headers,
            request_uri,
            target,
            path,
            ready: None,
            prewarm_attempted: false,
            prewarm_task: None,
            prewarm_rx,
            prewarm_tx,
            first_response_pending: true,
            force_http: false,
        }
    }

    fn start_prewarm(&mut self) {
        if self.prewarm_attempted || self.prewarm_task.is_some() {
            return;
        }
        self.prewarm_attempted = true;
        let target = self.target.clone();
        let headers = self.client_headers.clone();
        let tls_config = self.state.private_tls_config.clone();
        let sender = self.prewarm_tx.clone();
        self.prewarm_task = Some(tokio::spawn(async move {
            let upstream = private_websocket::connect_private(&target, &headers, &tls_config).await;
            let _ = sender.send(upstream).await;
        }));
    }

    fn finish_prewarm(&mut self, result: flow::PrewarmSelection) {
        self.prewarm_task.take();
        if let flow::PrewarmSelection::Ready(upstream) = result {
            self.state.metrics.record_websocket_connected();
            self.ready = Some(*upstream);
        }
    }

    fn abort_prewarm(&mut self) {
        if let Some(task) = self.prewarm_task.take() {
            task.abort();
        }
    }

    fn close_ready(&mut self) {
        if self.ready.take().is_some() {
            self.state.metrics.record_websocket_closed();
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActiveKind {
    Http,
    WebSocket,
}

struct Active {
    kind: ActiveKind,
    commands: mpsc::Sender<WorkerCommand>,
    events: mpsc::Receiver<WorkerEvent>,
    task: JoinHandle<()>,
}

enum WorkerCommand {
    Cancel(Vec<u8>),
    Forward(Vec<u8>, bool),
}

enum WorkerEvent {
    Message(Message),
    Terminal(Option<Box<PrivateWebSocket>>),
    Cancelled,
    Error { code: u16, message: &'static str },
}

enum ActiveSelection {
    Client(Option<Result<Message, WebSocketError>>),
    Worker(Option<Box<WorkerEvent>>),
}

async fn run_session(session: &mut Session, mut client: ClientWebSocket) {
    let mut active: Option<Active> = None;
    loop {
        if active.is_some() {
            let selection = {
                let Some(active_ref) = active.as_mut() else {
                    continue;
                };
                tokio::select! {
                    biased;
                    message = client.next() => ActiveSelection::Client(message),
                    event = active_ref.events.recv() => ActiveSelection::Worker(event.map(Box::new)),
                }
            };
            let keep_running = match selection {
                ActiveSelection::Client(message) => match active.as_mut() {
                    Some(active_ref) => {
                        active::handle_active_client_message(&mut client, active_ref, message).await
                    }
                    None => false,
                },
                ActiveSelection::Worker(event) => {
                    active::handle_worker_event(
                        &mut client,
                        session,
                        &mut active,
                        event.map(|event| *event),
                    )
                    .await
                }
            };
            if !keep_running {
                break;
            }
            continue;
        }

        let prewarm_enabled = session.prewarm_task.is_some();
        let ready_enabled = session.ready.is_some();
        let selection = flow::select_idle(
            client.next(),
            flow::receive_prewarm(&mut session.prewarm_rx),
            flow::poll_ready(&mut session.ready),
            prewarm_enabled,
            ready_enabled,
        )
        .await;
        let keep_running = match selection {
            flow::IdleSelection::Client(message) => {
                flow::handle_idle_client_message(&mut client, session, &mut active, message).await
            }
            flow::IdleSelection::Prewarm(result) => {
                session.finish_prewarm(result);
                true
            }
            flow::IdleSelection::Ready(result) => {
                active::handle_idle_upstream(&mut client, session, result).await
            }
        };
        if !keep_running {
            break;
        }
    }
    active::cleanup_session(session, &mut active);
}
