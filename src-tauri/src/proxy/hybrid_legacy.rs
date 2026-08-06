use std::{sync::Arc, time::Duration};

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{
        Message,
        protocol::{Role, WebSocketConfig},
    },
};

use super::{ClientWebSocket, PrivateWebSocket, Session, common::close_client, private_websocket};

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
        session.state.hybrid_pool.discard(&session.pool_scope).await;
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
    relay_standard_from_message(
        client,
        &mut upstream,
        legacy_message(payload, original_binary),
    )
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
        .checkout_wait(&session.pool_scope, Duration::from_secs(2))
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

async fn relay_standard_from_message(
    client: &mut ClientWebSocket,
    upstream: &mut ClientWebSocket,
    initial: Message,
) {
    if upstream.send(initial).await.is_err() {
        let _ = close_client(client, 1011, "websocket fallback send failed").await;
        return;
    }
    loop {
        tokio::select! {
            client_message = client.next() => {
                let Some(client_message) = client_message else { return; };
                let Ok(client_message) = client_message else { return; };
                let close = matches!(client_message, Message::Close(_));
                if upstream.send(client_message).await.is_err() || close {
                    return;
                }
            }
            upstream_message = upstream.next() => {
                let Some(upstream_message) = upstream_message else { return; };
                let Ok(upstream_message) = upstream_message else { return; };
                let close = matches!(upstream_message, Message::Close(_));
                if client.send(upstream_message).await.is_err() || close {
                    return;
                }
            }
        }
    }
}
