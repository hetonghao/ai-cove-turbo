use axum::{
    extract::Request as AxumRequest,
    http::{HeaderMap, StatusCode, Uri},
};
use hyper::{upgrade::OnUpgrade, upgrade::Upgraded};
use hyper_util::rt::TokioIo;
use tokio::{sync::mpsc, task::JoinHandle};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{
        Message,
        protocol::{Role, WebSocketConfig},
    },
};
use url::Url;

use super::{ProxyState, hybrid_pool::ConnectionActivity, private_websocket};

#[path = "hybrid_common.rs"]
mod common;
#[path = "hybrid_flow.rs"]
mod flow;
#[path = "hybrid_http.rs"]
mod http;
#[path = "hybrid_active.rs"]
mod idle;
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
#[path = "hybrid_session.rs"]
mod session;
#[path = "hybrid_sse.rs"]
mod sse;
#[cfg(test)]
#[path = "hybrid_tests.rs"]
mod tests;
#[path = "hybrid_transport_fallback.rs"]
mod transport_fallback;
#[path = "hybrid_websocket.rs"]
mod websocket;
#[path = "hybrid_worker.rs"]
mod worker;

use session::Session;

type ClientWebSocket = WebSocketStream<TokioIo<Upgraded>>;
type PrivateWebSocket = private_websocket::PrivateUpstream;

const WEBSOCKET_MESSAGE_LIMIT: usize = 128 * 1024 * 1024;
const MAX_HYBRID_WEBSOCKET_REQUEST_BYTES: usize = 15 * 1024 * 1024;

pub(super) fn spawn(state: ProxyState, request: &mut AxumRequest, target: Url, path: String) {
    let client_upgrade = hyper::upgrade::on(&mut *request);
    let client_headers = request.headers().clone();
    let request_uri = request.uri().clone();
    tokio::spawn(async move {
        Box::pin(run_after_upgrade(
            state,
            client_upgrade,
            client_headers,
            request_uri,
            target,
            path,
        ))
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
    let mut session = Session::open(state, client_headers, request_uri, target, path).await;
    session::run(&mut session, client).await;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActiveKind {
    Http,
    WebSocket,
}

struct Active {
    kind: ActiveKind,
    http_fallback: Option<Vec<u8>>,
    output_forwarded: bool,
    cancel_requested: bool,
    commands: mpsc::Sender<WorkerCommand>,
    events: mpsc::Receiver<WorkerEvent>,
    task: JoinHandle<()>,
}

#[derive(Clone, Copy, Debug, Default)]
struct WebSocketSendReceipt {
    raw_bytes: u64,
    sent_bytes: u64,
    compressed: bool,
}

struct TransportFallback {
    response: Message,
    code: u16,
    reason: String,
}

enum WorkerCommand {
    Cancel(Vec<u8>),
    Forward(Vec<u8>, bool),
}

enum WorkerEvent {
    Message(Message),
    WebSocketSent(WebSocketSendReceipt),
    Terminal {
        upstream: Option<Box<PrivateWebSocket>>,
        response_id: Option<String>,
    },
    FailedTerminal {
        response: Message,
        code: u16,
        reason: String,
    },
    TransportFallback(TransportFallback),
    Cancelled,
    Error {
        code: u16,
        message: String,
    },
}
