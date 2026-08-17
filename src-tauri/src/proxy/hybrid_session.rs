use axum::http::{HeaderMap, Uri};
use futures_util::StreamExt;
use tokio_tungstenite::tungstenite::{Error as WebSocketError, Message};
use url::Url;

use super::super::{
    ProxyState,
    hybrid_pool::{ConnectionActivity, ConnectionObservation, HybridScope, LeaseRetirement},
};
use super::{
    Active, ActiveKind, ClientWebSocket, PrivateWebSocket, WebSocketSendReceipt, WorkerEvent, flow,
    worker,
};

enum ActiveSelection {
    Client(Option<Result<Message, WebSocketError>>),
    Worker(Option<Box<WorkerEvent>>),
}

pub(super) struct Session {
    pub(super) state: ProxyState,
    pub(super) client_headers: HeaderMap,
    pub(super) request_uri: Uri,
    pub(super) target: Url,
    pub(super) path: String,
    pub(super) pool_scope: HybridScope,
    pub(super) ready: Option<PrivateWebSocket>,
    pub(super) websocket_receipt: Option<WebSocketSendReceipt>,
    pub(super) max_websocket_request_bytes: usize,
    pub(super) pool_id: u64,
    observed_activity: Option<ConnectionActivity>,
    pub(super) thread_id: Option<String>,
    pub(super) last_terminal_response_id: Option<String>,
    pub(super) response_started: bool,
    pub(super) drain_reconnect_pending: bool,
}

impl Session {
    // ponytail: upgrade yields five independent values; add a context type only for a second caller.
    pub(super) async fn open(
        state: ProxyState,
        client_headers: HeaderMap,
        request_uri: Uri,
        target: Url,
        path: String,
    ) -> Self {
        let pool_scope = HybridScope::new(&target, &client_headers);
        let pool_id = state
            .hybrid_pool
            .register(&pool_scope, target.clone(), client_headers.clone())
            .await;
        Self {
            state,
            client_headers,
            request_uri,
            target,
            path,
            pool_scope,
            ready: None,
            websocket_receipt: None,
            max_websocket_request_bytes: super::MAX_HYBRID_WEBSOCKET_REQUEST_BYTES,
            pool_id,
            observed_activity: None,
            thread_id: None,
            last_terminal_response_id: None,
            response_started: false,
            drain_reconnect_pending: false,
        }
    }

    pub(super) async fn bind_thread_id(&mut self, thread_id: Option<String>) -> bool {
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
            .observe_session(self.pool_id, ConnectionObservation::Bound { thread_id })
            .await;
        self.observed_activity = self.ready.as_ref().map(|_| ConnectionActivity::Idle);
        true
    }

    pub(super) async fn observe_activity(&mut self, activity: ConnectionActivity) {
        if self.thread_id.is_none() || self.observed_activity == Some(activity) {
            return;
        }
        self.state
            .hybrid_pool
            .observe_session(self.pool_id, ConnectionObservation::Active(activity))
            .await;
        self.observed_activity = Some(activity);
    }

    pub(super) async fn observe_idle(&mut self) {
        if self.thread_id.is_none() || self.observed_activity == Some(ConnectionActivity::Idle) {
            return;
        }
        self.state
            .hybrid_pool
            .observe_session(self.pool_id, ConnectionObservation::Idle)
            .await;
        self.observed_activity = Some(ConnectionActivity::Idle);
    }

    pub(super) async fn discard(&mut self, retirement: LeaseRetirement) {
        self.state
            .hybrid_pool
            .discard(&self.pool_scope, self.pool_id, retirement)
            .await;
        self.last_terminal_response_id = None;
        self.observed_activity = None;
    }

    pub(super) async fn retire_idle_upstream(&mut self, retirement: LeaseRetirement) {
        let upstream = self.ready.take();
        self.discard(retirement).await;
        if let Some(upstream) = upstream {
            self.state.hybrid_pool.close_all(vec![upstream]).await;
        }
    }
}

pub(super) async fn run(session: &mut Session, mut client: ClientWebSocket) {
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
                        worker::handle_active_client_message(
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
                    worker::handle_worker_event(
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

        if !flow::handle_idle(&mut client, session, &mut active).await {
            break;
        }
    }
    cleanup(session, &mut active).await;
}

async fn cleanup(session: &mut Session, active: &mut Option<Active>) {
    if let Some(active) = active.take() {
        active.task.abort();
        if active.kind == ActiveKind::WebSocket {
            session.state.metrics.record_websocket_closed();
            session
                .state
                .hybrid_pool
                .release_session_connection(&session.pool_scope, session.pool_id, None)
                .await;
        }
    }
    if let (Some(thread_id), Some(response_id)) = (
        session.thread_id.clone(),
        session.last_terminal_response_id.clone(),
    ) && let Some(upstream) = session.ready.take()
    {
        match session
            .state
            .hybrid_pool
            .park_session_connection(
                &session.pool_scope,
                session.pool_id,
                thread_id,
                response_id,
                upstream,
            )
            .await
        {
            Ok(()) => return,
            Err(upstream) => session.ready = Some(upstream),
        }
    }
    if let Some(upstream) = session.ready.take() {
        session
            .state
            .hybrid_pool
            .release_session_connection(&session.pool_scope, session.pool_id, Some(upstream))
            .await;
    }
    session
        .state
        .hybrid_pool
        .unregister(&session.pool_scope, session.pool_id)
        .await;
}
