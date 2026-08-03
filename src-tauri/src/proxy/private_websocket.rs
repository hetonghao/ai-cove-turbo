use std::time::Duration;

use axum::{
    body::Body,
    http::{HeaderMap, Response, StatusCode, header},
};
use tokio::net::TcpStream;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async_with_config,
    tungstenite::{
        client::IntoClientRequest, handshake::derive_accept_key, protocol::WebSocketConfig,
    },
};
use url::Url;

mod codec;
mod relay;

pub(super) use codec::{PRIVATE_ENVELOPE_HEADER_BYTES, PRIVATE_WEBSOCKET_SUBPROTOCOL};
#[cfg(test)]
pub(super) use codec::{decode_private_message, encode_private_message};
pub(super) use relay::relay_private;

use super::hop_by_hop_headers;
use codec::{PRIVATE_MESSAGE_MAX_BYTES, PrivateProtocolError};

const PRIVATE_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

pub(in crate::proxy) type PrivateUpstream = WebSocketStream<MaybeTlsStream<TcpStream>>;

pub(super) fn client_upgrade_response(
    headers: &HeaderMap,
) -> Result<Response<Body>, PrivateProtocolError> {
    if headers
        .get(header::SEC_WEBSOCKET_VERSION)
        .and_then(|value| value.to_str().ok())
        != Some("13")
    {
        return Err(PrivateProtocolError::protocol(
            "Sec-WebSocket-Version must be 13",
        ));
    }
    let key = headers
        .get(header::SEC_WEBSOCKET_KEY)
        .ok_or_else(|| PrivateProtocolError::protocol("Sec-WebSocket-Key is missing"))?;
    let accept = derive_accept_key(key.as_bytes());
    let accept = header::HeaderValue::from_str(&accept)
        .map_err(|_| PrivateProtocolError::internal("Sec-WebSocket-Accept is invalid"))?;
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
    Ok(response)
}

pub(super) async fn connect_private(
    target: &Url,
    client_headers: &HeaderMap,
) -> Option<PrivateUpstream> {
    let mut target = target.clone();
    let websocket_scheme = match target.scheme() {
        "http" => "ws",
        "https" => "wss",
        _ => return None,
    };
    if target.set_scheme(websocket_scheme).is_err() {
        return None;
    }
    let mut request = target.as_str().into_client_request().ok()?;
    let hop_by_hop = hop_by_hop_headers(client_headers);
    for (name, value) in client_headers {
        if !hop_by_hop.contains(name) && !is_client_handshake_header(name) {
            request.headers_mut().append(name, value.clone());
        }
    }
    request.headers_mut().insert(
        header::SEC_WEBSOCKET_PROTOCOL,
        header::HeaderValue::from_static(PRIVATE_WEBSOCKET_SUBPROTOCOL),
    );

    let message_limit = PRIVATE_MESSAGE_MAX_BYTES + PRIVATE_ENVELOPE_HEADER_BYTES;
    let config = WebSocketConfig::default()
        .max_message_size(Some(message_limit))
        .max_frame_size(Some(message_limit));
    let connected = tokio::time::timeout(
        PRIVATE_HANDSHAKE_TIMEOUT,
        connect_async_with_config(request, Some(config), false),
    )
    .await
    .ok()?
    .ok()?;
    let (mut stream, response) = connected;
    let accepted = response
        .headers()
        .get(header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|value| value.to_str().ok())
        == Some(PRIVATE_WEBSOCKET_SUBPROTOCOL)
        && !response
            .headers()
            .contains_key(header::SEC_WEBSOCKET_EXTENSIONS);
    if !accepted {
        let _ = stream.close(None).await;
        return None;
    }
    Some(stream)
}

fn is_client_handshake_header(name: &header::HeaderName) -> bool {
    *name == header::HOST
        || *name == header::CONNECTION
        || *name == header::UPGRADE
        || *name == header::CONTENT_LENGTH
        || *name == header::SEC_WEBSOCKET_KEY
        || *name == header::SEC_WEBSOCKET_VERSION
        || *name == header::SEC_WEBSOCKET_EXTENSIONS
        || *name == header::SEC_WEBSOCKET_PROTOCOL
}
