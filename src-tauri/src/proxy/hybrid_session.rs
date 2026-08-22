use axum::http::{HeaderMap, Uri};
use futures_util::StreamExt;
use tokio_tungstenite::tungstenite::{Error as WebSocketError, Message};
use url::Url;

use super::super::{
    ProxyState,
    hybrid_pool::{
        ConnectionActivity, ConnectionObservation, HybridScope, Lease, LeaseRetirement,
        SessionHandle,
    },
    model_policy::ModelPolicy,
    transport_capability::CapabilityHint,
};
use super::{Active, ActiveKind, ClientWebSocket, WebSocketSendReceipt, WorkerEvent, flow, worker};

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
    pub(super) handle: SessionHandle,
    pub(super) ready: Option<Lease>,
    pub(super) websocket_receipt: Option<WebSocketSendReceipt>,
    pub(super) max_websocket_request_bytes: usize,
    observed_activity: Option<ConnectionActivity>,
    pub(super) thread_id: Option<String>,
    pub(super) last_terminal_response_id: Option<String>,
    pub(super) response_started: bool,
    pub(super) drain_reconnect_pending: bool,
    pub(super) policy: ModelPolicy,
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
        let handle = state
            .hybrid_pool
            .open_session(&pool_scope, target.clone(), client_headers.clone())
            .await;
        let policy = state.model_policy.reload();
        Self {
            state,
            client_headers,
            request_uri,
            target,
            path,
            handle,
            ready: None,
            websocket_receipt: None,
            max_websocket_request_bytes: super::MAX_HYBRID_WEBSOCKET_REQUEST_BYTES,
            observed_activity: None,
            thread_id: None,
            last_terminal_response_id: None,
            response_started: false,
            drain_reconnect_pending: false,
            policy,
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
        self.handle
            .observe(ConnectionObservation::Bound { thread_id })
            .await;
        self.observed_activity = self.ready.as_ref().map(|_| ConnectionActivity::Idle);
        true
    }

    pub(super) async fn observe_activity(&mut self, activity: ConnectionActivity) {
        if self.thread_id.is_none() || self.observed_activity == Some(activity) {
            return;
        }
        self.handle
            .observe(ConnectionObservation::Active(activity))
            .await;
        self.observed_activity = Some(activity);
    }

    pub(super) async fn observe_idle(&mut self) {
        if self.thread_id.is_none() || self.observed_activity == Some(ConnectionActivity::Idle) {
            return;
        }
        self.handle.observe(ConnectionObservation::Idle).await;
        self.observed_activity = Some(ConnectionActivity::Idle);
    }

    pub(super) async fn discard(&mut self, retirement: LeaseRetirement) {
        if let Some(mut lease) = self.ready.take() {
            lease.discard(retirement.clone()).await;
        } else {
            self.handle.discard_unleased(retirement).await;
        }
        self.last_terminal_response_id = None;
        self.observed_activity = None;
    }

    pub(super) async fn capability_hint(&self, payload: &[u8]) -> Option<CapabilityHint> {
        if !self
            .state
            .upstream
            .host_str()
            .is_some_and(|host| host == "ai-cove.com" || host.ends_with(".ai-cove.com"))
        {
            return None;
        }
        let model = ModelPolicy::model_from_payload(payload)?;
        if let Some(hint) = self.state.capability_cache.hint(&model) {
            return Some(hint);
        }
        let mut models = self.policy.model_slugs();
        if !models.iter().any(|candidate| candidate == &model) {
            models.push(model.clone());
        }
        models.sort_unstable();
        models.dedup();
        if !self.state.capability_cache.needs_refresh(&models) {
            return None;
        }
        let headers = self.client_headers.clone();
        let result = super::super::transport_capability::fetch_batch(
            &self.state.client,
            &self.state.upstream,
            &headers,
            &models,
        )
        .await;
        match result {
            Ok(response) => {
                self.state
                    .capability_cache
                    .apply(&response, super::super::transport_capability::ttl());
            }
            Err(reason) => self
                .state
                .capability_cache
                .mark_attempt(&models, Some(reason.to_owned())),
        }
        self.state.capability_cache.hint(&model)
    }

    pub(super) async fn retire_idle_upstream(&mut self, retirement: LeaseRetirement) {
        self.discard(retirement).await;
    }
}

#[allow(clippy::large_futures)]
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
            session.handle.release_unleased().await;
        }
    }
    if let (Some(thread_id), Some(response_id)) = (
        session.thread_id.clone(),
        session.last_terminal_response_id.clone(),
    ) && let Some(mut lease) = session.ready.take()
    {
        if lease.park(thread_id, response_id).await.is_ok() {
            session.handle.detach_after_park();
            return;
        }
        session.ready = Some(lease);
    }
    if let Some(mut lease) = session.ready.take() {
        lease.release().await;
    }
    session.handle.close().await;
}
