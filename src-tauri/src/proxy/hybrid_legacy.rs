use std::{sync::Arc, time::Duration};

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{
        Message,
        protocol::{Role, WebSocketConfig},
    },
};

use super::sse::{is_success_terminal_event, is_terminal_event};
use super::{ClientWebSocket, PrivateWebSocket, Session, common::close_client, private_websocket};
use crate::proxy::{
    Metrics,
    traffic::{self, FailurePhase, TrafficRecord, TrafficResult, TrafficRoute, TrafficTransport},
};

pub(super) async fn start_legacy_response(
    client: &mut ClientWebSocket,
    session: &mut Session,
    payload: Vec<u8>,
    original_binary: bool,
) -> bool {
    if let Some(mut upstream) = take_private(session).await {
        private_websocket::relay_private_from_message(
            client,
            &mut upstream,
            legacy_message(payload, original_binary),
            Arc::clone(&session.state.metrics),
            session.path.clone(),
        )
        .await;
        session.state.metrics.record_websocket_closed();
        session
            .state
            .hybrid_pool
            .release_session_connection(&session.pool_scope, session.pool_id, None)
            .await;
        return false;
    }

    let Some(mut upstream) = connect_standard(session).await else {
        session
            .state
            .metrics
            .record_websocket_error(&session.path, super::StatusCode::BAD_GATEWAY.as_u16());
        let _ = close_client(client, 1011, "websocket fallback failed").await;
        return false;
    };
    session.state.metrics.record_websocket_connected();
    StandardRelay::new(&mut upstream, session.state.metrics.as_ref(), &session.path)
        .run(client, legacy_message(payload, original_binary))
        .await;
    session.state.metrics.record_websocket_closed();
    false
}

fn legacy_message(payload: Vec<u8>, original_binary: bool) -> Message {
    if original_binary {
        Message::Binary(payload.into())
    } else {
        Message::Text(String::from_utf8_lossy(&payload).into_owned().into())
    }
}

async fn take_private(session: &mut Session) -> Option<PrivateWebSocket> {
    if let Some(upstream) = session.ready.take() {
        return Some(upstream);
    }
    session
        .state
        .hybrid_pool
        .checkout_wait(&session.pool_scope, session.pool_id, Duration::from_secs(2))
        .await
}

async fn connect_standard(session: &Session) -> Option<ClientWebSocket> {
    let target_uri = session.target.as_str().parse().ok()?;
    let outbound = super::super::build_websocket_request(
        axum::http::Method::GET,
        &session.client_headers,
        target_uri,
    );
    let mut response = session
        .state
        .websocket_client
        .request(outbound)
        .await
        .ok()?;
    if response.status() != super::StatusCode::SWITCHING_PROTOCOLS {
        return None;
    }
    let upgraded = hyper::upgrade::on(&mut response).await.ok()?;
    Some(
        WebSocketStream::from_raw_socket(
            super::TokioIo::new(upgraded),
            Role::Client,
            Some(WebSocketConfig::default().max_message_size(Some(super::WEBSOCKET_MESSAGE_LIMIT))),
        )
        .await,
    )
}

struct StandardRelay<'a> {
    upstream: &'a mut ClientWebSocket,
    metrics: &'a Metrics,
    path: &'a str,
    active_request_bytes: Option<u64>,
}

impl<'a> StandardRelay<'a> {
    const fn new(upstream: &'a mut ClientWebSocket, metrics: &'a Metrics, path: &'a str) -> Self {
        Self {
            upstream,
            metrics,
            path,
            active_request_bytes: None,
        }
    }

    async fn run(&mut self, client: &mut ClientWebSocket, initial: Message) {
        if self.upstream.send(initial).await.is_err() {
            let _ = close_client(client, 1011, "websocket fallback send failed").await;
            return;
        }
        loop {
            tokio::select! {
                client_message = client.next() => {
                    let Some(client_message) = client_message else {
                        self.record_error(1006, "client websocket ended before terminal");
                        return;
                    };
                    let client_message = match client_message {
                        Ok(message) => message,
                        Err(error) => {
                            self.record_error(
                                private_websocket::websocket_error_code(&error),
                                "client websocket failed before terminal",
                            );
                            return;
                        }
                    };
                    let close_status = close_status(&client_message);
                    let request_bytes = application_payload(&client_message)
                        .filter(|payload| {
                            super::common::event_type(payload)
                                .is_ok_and(|event_type| event_type == "response.create")
                        })
                        .map(|payload| payload.len() as u64);
                    if self.upstream.send(client_message).await.is_err() {
                        self.record_error(1011, "standard websocket send failed before terminal");
                        return;
                    }
                    if let Some(status) = close_status {
                        self.record_error(status, "client websocket closed before terminal");
                        return;
                    }
                    if let Some(request_bytes) = request_bytes {
                        self.active_request_bytes = Some(request_bytes);
                    }
                }
                upstream_message = self.upstream.next() => {
                    let Some(upstream_message) = upstream_message else {
                        self.record_error(1006, "standard websocket ended before terminal");
                        return;
                    };
                    let upstream_message = match upstream_message {
                        Ok(message) => message,
                        Err(error) => {
                            self.record_error(
                                private_websocket::websocket_error_code(&error),
                                "standard websocket failed before terminal",
                            );
                            return;
                        }
                    };
                    let close_status = close_status(&upstream_message);
                    let payload = application_payload(&upstream_message);
                    let success = payload.is_some_and(is_success_terminal_event);
                    let terminal = payload.is_some_and(is_terminal_event);
                    if let Some(status) = close_status {
                        self.record_error(status, "standard websocket closed before terminal");
                    }
                    if client.send(upstream_message).await.is_err() {
                        self.record_error(1011, "client websocket send failed before terminal");
                        return;
                    }
                    if close_status.is_some() {
                        return;
                    }
                    if success {
                        self.record_success();
                    } else if terminal {
                        self.record_error(1011, "upstream returned a failed terminal event");
                    }
                }
            }
        }
    }

    fn record_success(&mut self) {
        self.record_outcome(super::StatusCode::SWITCHING_PROTOCOLS.as_u16(), None);
    }

    fn record_error(&mut self, status: u16, reason: &str) {
        self.record_outcome(status, Some(reason));
    }

    fn record_outcome(&mut self, status: u16, failure_reason: Option<&str>) {
        let Some(bytes) = self.active_request_bytes.take() else {
            return;
        };
        let (result, failure_phase) = match failure_reason {
            Some(_) => (TrafficResult::Error, Some(FailurePhase::HybridActive)),
            None => (TrafficResult::Success, None),
        };
        self.metrics.record_websocket_outcome(
            TrafficRecord {
                timestamp_ms: traffic::now_ms(),
                status,
                path: self.path,
                raw_bytes: bytes,
                sent_bytes: bytes,
                transport: TrafficTransport::Ws,
                result,
                route: Some(TrafficRoute::HybridWs),
                failure_phase,
                failure_reason,
            },
            false,
        );
    }
}

fn application_payload(message: &Message) -> Option<&[u8]> {
    match message {
        Message::Text(text) => Some(text.as_bytes()),
        Message::Binary(payload) => Some(payload.as_ref()),
        Message::Ping(_) | Message::Pong(_) | Message::Close(_) | Message::Frame(_) => None,
    }
}

fn close_status(message: &Message) -> Option<u16> {
    match message {
        Message::Close(frame) => Some(frame.as_ref().map_or(1006, |frame| u16::from(frame.code))),
        Message::Text(_)
        | Message::Binary(_)
        | Message::Ping(_)
        | Message::Pong(_)
        | Message::Frame(_) => None,
    }
}
