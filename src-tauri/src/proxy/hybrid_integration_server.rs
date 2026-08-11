use axum::{
    body::{Body, to_bytes},
    extract::{Request, State},
    http::{Response, StatusCode, header},
};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{
        Bytes, Message,
        handshake::derive_accept_key,
        protocol::{CloseFrame, Role, frame::coding::CloseCode},
    },
};

use super::integration_state::{Fixture, PrivateBehavior};
use crate::proxy::{decode_private_message, encode_private_message};

type FixtureWebSocket = WebSocketStream<hyper_util::rt::TokioIo<hyper::upgrade::Upgraded>>;

const ACTIVE_READY_PAYLOAD: &[u8] = b"fixture-active-ready";

pub(super) async fn upstream_request(
    State(fixture): State<Fixture>,
    mut request: Request,
) -> Response<Body> {
    let private = request
        .headers()
        .get(header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == "ai-cove-zstd.v1");
    let upgrade = request
        .headers()
        .get(header::UPGRADE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("websocket"));
    if upgrade {
        return upgrade_response(&fixture, &mut request, private).await;
    }
    let Ok(_) = to_bytes(request.into_body(), 1024 * 1024).await else {
        return Response::new(Body::empty());
    };
    fixture.record(|counts| counts.http_requests += 1).await;
    if fixture.config.delay_http {
        fixture.state.release_http.notified().await;
    }
    let mut response = Response::new(Body::from("data: {\"type\":\"response.completed\"}\n\n"));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("text/event-stream"),
    );
    response
}

impl Fixture {
    async fn send_private_event(websocket: &mut FixtureWebSocket, event: &[u8]) -> bool {
        let Ok(encoded) = encode_private_message(event, false) else {
            return false;
        };
        websocket
            .send(Message::Binary(Bytes::from(encoded)))
            .await
            .is_ok()
    }

    async fn close_upstream(
        &self,
        websocket: &mut FixtureWebSocket,
        code: CloseCode,
        reason: &'static str,
    ) -> bool {
        let sent = websocket
            .send(Message::Close(Some(CloseFrame {
                code,
                reason: reason.into(),
            })))
            .await
            .is_ok();
        if sent {
            self.record(|counts| counts.close_frames_sent += 1).await;
        }
        sent
    }

    async fn send_replay_required_failure(&self, websocket: &mut FixtureWebSocket) {
        let failed = br#"{"type":"error","error":{"code":"upstream_error","message":"upstream requires HTTP replay"}}"#;
        if Self::send_private_event(websocket, failed).await {
            let _ = self
                .close_upstream(
                    websocket,
                    CloseCode::Restart,
                    "upstream requires HTTP replay",
                )
                .await;
        }
    }

    async fn send_idle_event(&self, websocket: &mut FixtureWebSocket) -> bool {
        let event = if matches!(self.config.private, PrivateBehavior::IdleError) {
            br#"{"type":"error","error":{"code":"do_request_failed","message":"simulated idle failure"}}"#
                .as_slice()
        } else {
            br#"{"type":"response.output_text.delta","secret":"must-not-persist"}"#.as_slice()
        };
        let _ = Self::send_private_event(websocket, event).await;
        true
    }

    async fn receive_private_request(
        &self,
        websocket: &mut FixtureWebSocket,
        previous_response_id: Option<&str>,
    ) -> bool {
        loop {
            let payload = match websocket.next().await {
                Some(Ok(Message::Binary(payload))) => payload,
                Some(Ok(Message::Ping(payload))) => {
                    if websocket.send(Message::Pong(payload)).await.is_err() {
                        return false;
                    }
                    continue;
                }
                Some(Ok(Message::Close(frame))) => {
                    if frame
                        .as_ref()
                        .is_some_and(|frame| frame.code == CloseCode::Normal)
                    {
                        self.record(|counts| counts.private_normal_closes += 1)
                            .await;
                    }
                    return false;
                }
                _ => return false,
            };
            let Ok(decoded) = decode_private_message(&payload) else {
                return false;
            };
            self.record(|counts| counts.private_messages += 1).await;
            if !stateful_previous_response_mismatch(
                self.config.private,
                &decoded.payload,
                previous_response_id,
            ) {
                return true;
            }
            let missing = br#"{"type":"error","status":409,"error":{"code":"previous_response_not_found","message":"Previous response is not available on this websocket"}}"#;
            if !Self::send_private_event(websocket, missing).await {
                return false;
            }
        }
    }

    async fn wait_for_active_release(&self, websocket: &mut FixtureWebSocket) -> bool {
        loop {
            tokio::select! {
                () = self.state.release_private.notified() => return true,
                message = websocket.next() => match message {
                    Some(Ok(Message::Ping(payload))) => {
                        self.record(|counts| counts.active_pings += 1).await;
                        if websocket.send(Message::Pong(payload)).await.is_err() {
                            return false;
                        }
                    }
                    _ => return false,
                }
            }
        }
    }

    async fn prepare_active_hold(&self, websocket: &mut FixtureWebSocket) -> bool {
        let payload = Bytes::from_static(ACTIVE_READY_PAYLOAD);
        if websocket
            .send(Message::Ping(payload.clone()))
            .await
            .is_err()
        {
            return false;
        }
        loop {
            match websocket.next().await {
                Some(Ok(Message::Pong(received))) if received == payload => {
                    self.record(|counts| counts.active_ready += 1).await;
                    return true;
                }
                Some(Ok(Message::Ping(received))) => {
                    if websocket.send(Message::Pong(received)).await.is_err() {
                        return false;
                    }
                }
                _ => return false,
            }
        }
    }

    async fn hold_active_without_pong(&self, mut websocket: FixtureWebSocket) {
        let Some(Ok(Message::Ping(_))) = websocket.next().await else {
            return;
        };
        let upstream = websocket.into_inner();
        self.record(|counts| counts.active_pings += 1).await;
        self.state.release_private.notified().await;
        drop(upstream);
    }

    async fn run_private(&self, mut websocket: FixtureWebSocket) {
        let mut previous_response_id: Option<String> = None;
        let mut cancelled_on_connection = false;
        loop {
            if !self
                .receive_private_request(&mut websocket, previous_response_id.as_deref())
                .await
            {
                return;
            }
            let response_index = self.counts().await.private_messages;
            match (self.config.private, response_index) {
                (PrivateBehavior::ActiveFailure, 1) => return,
                (PrivateBehavior::ActiveReplayRequired, 1) => {
                    self.send_replay_required_failure(&mut websocket).await;
                    return;
                }
                (PrivateBehavior::CancelledTerminal, 1) => {
                    cancelled_on_connection = Self::send_private_event(
                        &mut websocket,
                        br#"{"type":"response.cancelled","response":{"status":"cancelled"}}"#,
                    )
                    .await;
                    if !cancelled_on_connection {
                        return;
                    }
                    continue;
                }
                (PrivateBehavior::CancelledTerminal, _) if !cancelled_on_connection => return,
                _ => {}
            }
            if self.config.private.holds_response()
                && !self.prepare_active_hold(&mut websocket).await
            {
                return;
            }
            if matches!(self.config.private, PrivateBehavior::HoldResponse)
                && !self.wait_for_active_release(&mut websocket).await
            {
                return;
            }
            if matches!(self.config.private, PrivateBehavior::HoldResponseNoPong)
                && response_index == 1
            {
                self.hold_active_without_pong(websocket).await;
                return;
            }
            if matches!(self.config.private, PrivateBehavior::TerminalTail)
                && let Some(response_id) = previous_response_id.as_deref()
            {
                let tail = serde_json::json!({
                    "type": "response.done",
                    "response": {"id": response_id},
                    "timing": {"upstream_ms": 1},
                    "metadata": {"source": "fixture"},
                });
                let Ok(tail) = serde_json::to_vec(&tail) else {
                    return;
                };
                if !Self::send_private_event(&mut websocket, &tail).await {
                    return;
                }
            }
            let response_id = format!("response-{response_index}");
            let completed = serde_json::json!({
                "type": "response.completed",
                "response": {"id": response_id},
            });
            let Ok(completed) = serde_json::to_vec(&completed) else {
                return;
            };
            if !Self::send_private_event(&mut websocket, &completed).await {
                return;
            }
            if matches!(
                self.config.private,
                PrivateBehavior::IdleError | PrivateBehavior::IdleMessage
            ) && !self.send_idle_event(&mut websocket).await
            {
                return;
            }
            previous_response_id = Some(response_id);
            if let Some((code, reason)) = self.config.private.idle_close() {
                let _ = self
                    .close_upstream(&mut websocket, CloseCode::from(code), reason)
                    .await;
            }
            if !self.config.private.keeps_connection_open() {
                return;
            }
        }
    }
}

fn stateful_previous_response_mismatch(
    behavior: PrivateBehavior,
    payload: &[u8],
    previous_response_id: Option<&str>,
) -> bool {
    matches!(behavior, PrivateBehavior::Stateful)
        && serde_json::from_slice::<serde_json::Value>(payload)
            .ok()
            .and_then(|request| {
                request
                    .get("previous_response_id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            })
            .is_some_and(|requested| previous_response_id != Some(&requested))
}

async fn upgrade_response(
    fixture: &Fixture,
    request: &mut Request,
    private: bool,
) -> Response<Body> {
    fixture
        .record(|counts| counts.private_handshakes += 1)
        .await;
    let private_handshakes = fixture.counts().await.private_handshakes;
    if private
        && (matches!(fixture.config.private, PrivateBehavior::Fail)
            || matches!(fixture.config.private, PrivateBehavior::FailFirstBatch)
                && private_handshakes <= 6)
    {
        let mut response = Response::new(Body::empty());
        *response.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
        return response;
    }
    if private
        && (matches!(fixture.config.private, PrivateBehavior::Delay)
            || matches!(
                fixture.config.private,
                PrivateBehavior::IdleRestartDelayedReconnect
            ) && private_handshakes > 1)
    {
        fixture.state.release_private.notified().await;
    }
    let Some(key) = request.headers().get(header::SEC_WEBSOCKET_KEY) else {
        return Response::new(Body::empty());
    };
    let accept = derive_accept_key(key.as_bytes());
    let Ok(accept) = header::HeaderValue::from_str(&accept) else {
        return Response::new(Body::empty());
    };
    let upgrade = hyper::upgrade::on(request);
    let worker = fixture.clone();
    tokio::spawn(async move {
        let Ok(upgraded) = upgrade.await else {
            return;
        };
        worker.record(|counts| counts.private_ready += 1).await;
        let websocket = WebSocketStream::from_raw_socket(
            hyper_util::rt::TokioIo::new(upgraded),
            Role::Server,
            None,
        )
        .await;
        worker.run_private(websocket).await;
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
    if private {
        response.headers_mut().insert(
            header::SEC_WEBSOCKET_PROTOCOL,
            header::HeaderValue::from_static("ai-cove-zstd.v1"),
        );
    }
    response
}
