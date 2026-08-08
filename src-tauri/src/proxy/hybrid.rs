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

use super::{
    ProxyState,
    hybrid_pool::{ConnectionActivity, ConnectionObservation, HybridScope},
    private_websocket,
};

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
    let pool_scope = HybridScope::new(&target, &client_headers);
    state
        .hybrid_pool
        .register(&pool_scope, target.clone(), client_headers.clone())
        .await;
    let observation_id = state.hybrid_pool.register_observed_session().await;
    let ready = state.hybrid_pool.checkout(&pool_scope).await;
    let mut session = Session {
        state,
        client_headers,
        request_uri,
        target,
        path,
        pool_scope,
        ready,
        observation_id,
        observed_activity: None,
        thread_id: None,
        last_terminal_response_id: None,
    };
    run_session(&mut session, client).await;
}

struct Session {
    state: ProxyState,
    client_headers: HeaderMap,
    request_uri: Uri,
    target: Url,
    path: String,
    pool_scope: HybridScope,
    ready: Option<PrivateWebSocket>,
    observation_id: u64,
    observed_activity: Option<ConnectionActivity>,
    thread_id: Option<String>,
    last_terminal_response_id: Option<String>,
}

impl Session {
    async fn bind_thread_id(&mut self, thread_id: Option<String>) -> bool {
        match (&self.thread_id, thread_id) {
            (Some(current), Some(next)) => return current == &next,
            (None, Some(next)) => {
                self.thread_id = Some(next);
            }
            (Some(_) | None, None) => return true,
        }
        let Some(thread_id) = self.thread_id.clone() else {
            return true;
        };
        self.state
            .hybrid_pool
            .observe_session(
                self.observation_id,
                ConnectionObservation::Bound {
                    thread_id,
                    has_connection: self.ready.is_some(),
                },
            )
            .await;
        self.observed_activity = self.ready.as_ref().map(|_| ConnectionActivity::Idle);
        true
    }

    async fn observe_activity(&mut self, activity: ConnectionActivity) {
        if self.thread_id.is_none() || self.observed_activity == Some(activity) {
            return;
        }
        self.state
            .hybrid_pool
            .observe_session(self.observation_id, ConnectionObservation::Active(activity))
            .await;
        self.observed_activity = Some(activity);
    }

    async fn observe_idle(&mut self) {
        if self.thread_id.is_none() || self.observed_activity == Some(ConnectionActivity::Idle) {
            return;
        }
        self.state
            .hybrid_pool
            .observe_session(self.observation_id, ConnectionObservation::Idle)
            .await;
        self.observed_activity = Some(ConnectionActivity::Idle);
    }

    async fn observe_recovering(&mut self, reason: impl Into<String>) {
        if self.thread_id.is_none() {
            return;
        }
        self.state
            .hybrid_pool
            .observe_session(
                self.observation_id,
                ConnectionObservation::Recovering {
                    reason: reason.into(),
                },
            )
            .await;
        self.observed_activity = None;
    }

    async fn observe_closed(&mut self) {
        self.state
            .hybrid_pool
            .observe_session(
                self.observation_id,
                ConnectionObservation::Closed {
                    reason: "Codex 线程结束".to_owned(),
                    normal: true,
                },
            )
            .await;
        self.observed_activity = None;
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
    Terminal {
        upstream: Option<Box<PrivateWebSocket>>,
        response_id: Option<String>,
    },
    Cancelled,
    Error {
        code: u16,
        message: &'static str,
    },
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
                        active::handle_active_client_message(
                            &mut client,
                            session,
                            active_ref,
                            message,
                        )
                        .await
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

        let ready_enabled = session.ready.is_some();
        let selection = flow::select_idle(
            client.next(),
            flow::poll_ready(&mut session.ready),
            tokio::time::sleep(super::hybrid_pool::KEEPALIVE_INTERVAL),
            ready_enabled,
        )
        .await;
        let keep_running = match selection {
            flow::IdleSelection::Client(message) => {
                flow::handle_idle_client_message(&mut client, session, &mut active, message).await
            }
            flow::IdleSelection::Ready(result) => {
                active::handle_idle_upstream(&mut client, session, result).await
            }
            flow::IdleSelection::Keepalive => active::handle_idle_keepalive(session).await,
        };
        if !keep_running {
            break;
        }
    }
    cleanup_session(session, &mut active).await;
}

async fn cleanup_session(session: &mut Session, active: &mut Option<Active>) {
    if let Some(active) = active.take() {
        active.task.abort();
        if active.kind == ActiveKind::WebSocket {
            session.state.metrics.record_websocket_closed();
            session
                .state
                .hybrid_pool
                .release_session_connection(&session.pool_scope, None)
                .await;
        }
    }
    if let Some(upstream) = session.ready.take() {
        session
            .state
            .hybrid_pool
            .release_session_connection(&session.pool_scope, Some(upstream))
            .await;
    }
    session
        .state
        .hybrid_pool
        .unregister(&session.pool_scope)
        .await;
    session.observe_closed().await;
}
