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
    async fn run_private(
        &self,
        mut websocket: WebSocketStream<hyper_util::rt::TokioIo<hyper::upgrade::Upgraded>>,
    ) {
        let Some(Ok(Message::Binary(payload))) = websocket.next().await else {
            return;
        };
        if decode_private_message(&payload).is_err() {
            return;
        }
        self.record(|counts| counts.private_messages += 1).await;
        if matches!(self.config.private, PrivateBehavior::ActiveFailure)
            && self.counts().await.private_messages == 1
        {
            return;
        }
        let Ok(envelope) = encode_private_message(br#"{"type":"response.completed"}"#, false)
        else {
            return;
        };
        if websocket
            .send(Message::Binary(Bytes::from(envelope)))
            .await
            .is_err()
        {
            return;
        }
        if matches!(self.config.private, PrivateBehavior::IdleRestart) {
            let _ = websocket
                .send(Message::Close(Some(CloseFrame {
                    code: CloseCode::from(1012),
                    reason: "restart".into(),
                })))
                .await;
            self.record(|counts| counts.idle_restarts += 1).await;
        }
    }
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
            || matches!(fixture.config.private, PrivateBehavior::FailOnce)
                && private_handshakes == 1)
    {
        let mut response = Response::new(Body::empty());
        *response.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
        return response;
    }
    if matches!(fixture.config.private, PrivateBehavior::Delay) && private {
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
