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

use super::{ProxyState, hybrid_pool::HybridScope, private_websocket};

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
    let ready = state.hybrid_pool.checkout(&pool_scope).await;
    let mut session = Session {
        state,
        client_headers,
        request_uri,
        target,
        path,
        pool_scope,
        ready,
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
    active::cleanup_session(session, &mut active).await;
}
