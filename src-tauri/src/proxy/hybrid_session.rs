use axum::http::{HeaderMap, Uri};
use futures_util::StreamExt;
use serde_json::Value;
use tokio_tungstenite::tungstenite::{Error as WebSocketError, Message};
use url::Url;

use super::super::continuation_merge::{HttpContinuation, PendingHttpContinuation};
use super::super::{
    HttpTraffic, ProxyState,
    hybrid_pool::{
        ConnectionActivity, ConnectionObservation, HybridScope, Lease, LeaseRetirement,
        SessionHandle,
    },
    model_policy::ModelPolicy,
    traffic::RequestMetadata,
};
use super::{
    Active, ActiveKind, ClientWebSocket, ResponseTransport, WebSocketSendReceipt, WorkerEvent,
    flow, worker,
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
    pub(super) handle: SessionHandle,
    pub(super) ready: Option<Lease>,
    pub(super) websocket_receipt: Option<WebSocketSendReceipt>,
    pub(super) websocket_first_frame_at: Option<std::time::Instant>,
    pub(super) websocket_first_token_at: Option<std::time::Instant>,
    pub(super) max_websocket_request_bytes: usize,
    observed_activity: Option<ConnectionActivity>,
    pub(super) thread_id: Option<String>,
    pub(super) request_metadata: Option<RequestMetadata>,
    pub(super) connection_id: Option<String>,
    pub(super) last_terminal_response_id: Option<String>,
    pub(super) last_response_transport: Option<ResponseTransport>,
    pub(super) last_http_traffic: Option<HttpTraffic>,
    pub(super) last_deepseek_tool_calls: Vec<Value>,
    pub(super) pending_deepseek_tool_calls: Vec<Value>,
    pub(super) capture_deepseek_tool_calls: bool,
    pub(super) pending_http_continuation: Option<PendingHttpContinuation>,
    pub(super) last_http_continuation: Option<HttpContinuation>,
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
            websocket_first_frame_at: None,
            websocket_first_token_at: None,
            max_websocket_request_bytes: super::MAX_HYBRID_WEBSOCKET_REQUEST_BYTES,
            observed_activity: None,
            thread_id: None,
            request_metadata: None,
            connection_id: None,
            last_terminal_response_id: None,
            last_response_transport: None,
            last_http_traffic: None,
            last_deepseek_tool_calls: Vec::new(),
            pending_deepseek_tool_calls: Vec::new(),
            capture_deepseek_tool_calls: false,
            pending_http_continuation: None,
            last_http_continuation: None,
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
        self.last_response_transport = None;
        self.last_http_traffic = None;
        self.last_deepseek_tool_calls.clear();
        self.pending_deepseek_tool_calls.clear();
        self.capture_deepseek_tool_calls = false;
        self.clear_http_continuation();
        self.websocket_first_frame_at = None;
        self.websocket_first_token_at = None;
        self.observed_activity = None;
    }

    /// 记录一次响应事件：WebSocket 响应只采集 `DeepSeek` 历史修复所需的 tool call，
    /// HTTP 响应额外累积状态续传展开所需的 output items。
    pub(super) fn observe_response_event(
        &mut self,
        message: &tokio_tungstenite::tungstenite::Message,
        from_websocket: bool,
    ) {
        self.observe_deepseek_tool_event(message);
        if !from_websocket {
            self.observe_http_response_event(message);
        }
    }

    fn observe_deepseek_tool_event(&mut self, message: &tokio_tungstenite::tungstenite::Message) {
        if !self.capture_deepseek_tool_calls {
            return;
        }
        let Some(payload) = message_bytes(message) else {
            return;
        };
        super::super::deepseek_history::upsert_tool_calls(
            &mut self.pending_deepseek_tool_calls,
            super::super::deepseek_history::tool_calls_from_event(payload),
        );
    }

    /// 记录即将发往上游的 HTTP 请求体，作为之后展开状态续传的底稿。
    pub(super) fn begin_http_continuation(&mut self, payload: &[u8]) {
        self.pending_http_continuation = Some(PendingHttpContinuation::new(payload));
    }

    /// 累积本轮 HTTP 响应里可以进入下一次请求的 output items。
    fn observe_http_response_event(&mut self, message: &tokio_tungstenite::tungstenite::Message) {
        let Some(payload) = message_bytes(message) else {
            return;
        };
        let Some(pending) = self.pending_http_continuation.as_mut() else {
            return;
        };
        pending.observe(payload);
    }

    /// 终态事件到达后固化续传材料；没有响应 ID 的响应不允许被续传。
    pub(super) fn commit_http_continuation(&mut self, response_id: Option<&str>) {
        let pending = self.pending_http_continuation.take();
        self.last_http_continuation = match response_id {
            Some(_) => pending.and_then(PendingHttpContinuation::finish),
            None => None,
        };
    }

    /// 本轮 HTTP 请求没有拿到可用终态，丢弃未完成的底稿。
    pub(super) fn abort_http_continuation(&mut self) {
        self.pending_http_continuation = None;
    }

    pub(super) fn clear_http_continuation(&mut self) {
        self.pending_http_continuation = None;
        self.last_http_continuation = None;
    }

    /// 把带 `previous_response_id` 的续传帧展开成自包含请求；状态对不上时不展开。
    pub(super) fn expand_http_continuation(
        &self,
        payload: &[u8],
        previous_response_id: Option<&str>,
    ) -> Option<Vec<u8>> {
        let previous_response_id = previous_response_id?;
        if self.last_terminal_response_id.as_deref() != Some(previous_response_id) {
            return None;
        }
        self.last_http_continuation.as_ref()?.expand(payload)
    }

    pub(super) fn commit_deepseek_tool_calls(&mut self) {
        self.last_deepseek_tool_calls = std::mem::take(&mut self.pending_deepseek_tool_calls);
    }

    pub(super) fn clear_pending_deepseek_tool_calls(&mut self) {
        self.pending_deepseek_tool_calls.clear();
    }

    pub(super) fn refresh_capability(&self, payload: &[u8]) {
        if !self
            .state
            .upstream
            .host_str()
            .is_some_and(|host| host == "ai-cove.com" || host.ends_with(".ai-cove.com"))
        {
            return;
        }
        let Some(model) = ModelPolicy::model_from_payload(payload) else {
            return;
        };
        if !self.state.capability_cache.needs_refresh_for(&model) {
            return;
        }
        let mut models = self.policy.model_slugs();
        if !models.iter().any(|candidate| candidate == &model) {
            models.push(model);
        }
        models.sort_unstable();
        models.dedup();
        self.state.capability_probe.refresh(&models);
    }

    pub(super) fn auto_uses_http(&self, payload: &[u8]) -> bool {
        let Some(model) = ModelPolicy::model_from_payload(payload) else {
            return self.is_ai_cove_upstream();
        };
        if let Some(transport) = self.state.capability_cache.known_transport_for(&model) {
            return transport == super::super::transport_capability::CapabilityTransport::HttpOnly;
        }
        self.is_ai_cove_upstream()
    }

    fn is_ai_cove_upstream(&self) -> bool {
        self.state
            .upstream
            .host_str()
            .is_some_and(|host| host == "ai-cove.com" || host.ends_with(".ai-cove.com"))
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
    if session.last_response_transport == Some(ResponseTransport::WebSocket)
        && let (Some(thread_id), Some(response_id)) = (
            session.thread_id.clone(),
            session.last_terminal_response_id.clone(),
        )
        && let Some(mut lease) = session.ready.take()
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

fn message_bytes(message: &Message) -> Option<&[u8]> {
    match message {
        Message::Text(text) => Some(text.as_bytes()),
        Message::Binary(payload) => Some(payload.as_ref()),
        _ => None,
    }
}
