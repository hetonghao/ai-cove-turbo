use std::{fmt, sync::Arc, time::Duration};
#[cfg(test)]
#[path = "private_websocket_test.rs"]
mod tests;
use axum::{
    body::Body,
    http::{HeaderMap, HeaderValue, Response, StatusCode, header},
};
use rustls::ClientConfig;
use tokio::{net::TcpStream, task::JoinError};
use tokio_tungstenite::{
    Connector, MaybeTlsStream, WebSocketStream, connect_async_tls_with_config,
    tungstenite::{
        Bytes, Error as WebSocketError, client::IntoClientRequest, handshake::derive_accept_key,
        protocol::WebSocketConfig,
    },
};
use url::Url;

mod codec;
mod relay;

pub(super) use codec::{
    DecodedPrivateMessage, EncodedPrivateMessage, PRIVATE_ENVELOPE_HEADER_BYTES,
    PRIVATE_WEBSOCKET_SUBPROTOCOL,
};
#[cfg(test)]
pub(super) use codec::{decode_private_message, encode_private_message};
pub(super) use relay::{relay_private_from_message, websocket_error_code};

use super::hop_by_hop_headers;
use codec::{PRIVATE_MESSAGE_MAX_BYTES, PrivateProtocolError};

const PRIVATE_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const PRIVATE_CLOSE_TIMEOUT: Duration = Duration::from_secs(10);
pub(super) const WS_TRACE_HEADER: &str = "x-ai-cove-ws-trace";

fn valid_server_trace(value: &HeaderValue) -> Option<String> {
    let value = value.to_str().ok()?;
    (value.len() == 32
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')))
    .then(|| value.to_owned())
}

pub(super) const fn should_offload_private_encoding(payload_len: usize) -> bool {
    payload_len >= super::MIN_COMPRESSION_INPUT_BYTES
}

pub(super) async fn encode_private_message_async(
    payload: Vec<u8>,
    original_binary: bool,
) -> Result<EncodedPrivateMessage, PrivateProtocolError> {
    if should_offload_private_encoding(payload.len()) {
        return tokio::task::spawn_blocking(move || {
            codec::encode_private_message_with_metadata(&payload, original_binary)
        })
        .await
        .map_err(join_error)?;
    }
    codec::encode_private_message_with_metadata(&payload, original_binary)
}

pub(super) async fn decode_private_message_async(
    envelope: Bytes,
) -> Result<codec::DecodedPrivateMessage, PrivateProtocolError> {
    tokio::task::spawn_blocking(move || codec::decode_private_message(&envelope))
        .await
        .map_err(join_error)?
}

fn join_error(_: JoinError) -> PrivateProtocolError {
    PrivateProtocolError::internal("private websocket worker failed")
}

#[derive(Clone, Debug)]
pub(super) struct PrivateTlsConfig(Arc<ClientConfig>);

impl PrivateTlsConfig {
    pub(super) const fn new(config: Arc<ClientConfig>) -> Self {
        Self(config)
    }

    fn connector(&self) -> Connector {
        Connector::Rustls(Arc::clone(&self.0))
    }
}

pub(in crate::proxy) type PrivateUpstream = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PrivateConnectFailure {
    InvalidTarget,
    Timeout,
    Http(u16),
    Network,
    Tls,
    Protocol,
    PrivateProtocolRejected,
}

impl fmt::Display for PrivateConnectFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTarget => formatter.write_str("目标地址无效"),
            Self::Timeout => formatter.write_str("握手超时"),
            Self::Http(status) => write!(formatter, "HTTP {status}"),
            Self::Network => formatter.write_str("网络连接失败"),
            Self::Tls => formatter.write_str("TLS 握手失败"),
            Self::Protocol => formatter.write_str("WebSocket 协议失败"),
            Self::PrivateProtocolRejected => formatter.write_str("上游未接受 Turbo 私有协议"),
        }
    }
}

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
    tls_config: &PrivateTlsConfig,
) -> Result<(PrivateUpstream, Option<String>), PrivateConnectFailure> {
    let mut target = target.clone();
    let websocket_scheme = match target.scheme() {
        "http" => "ws",
        "https" => "wss",
        _ => return Err(PrivateConnectFailure::InvalidTarget),
    };
    target
        .set_scheme(websocket_scheme)
        .map_err(|()| PrivateConnectFailure::InvalidTarget)?;
    let mut request = target
        .as_str()
        .into_client_request()
        .map_err(|_| PrivateConnectFailure::InvalidTarget)?;
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
        connect_async_tls_with_config(request, Some(config), false, Some(tls_config.connector())),
    )
    .await
    .map_err(|_| PrivateConnectFailure::Timeout)?
    .map_err(classify_connect_failure)?;
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
        let _ = tokio::time::timeout(PRIVATE_CLOSE_TIMEOUT, stream.close(None)).await;
        return Err(PrivateConnectFailure::PrivateProtocolRejected);
    }
    let server_trace = response
        .headers()
        .get(WS_TRACE_HEADER)
        .and_then(valid_server_trace);
    Ok((stream, server_trace))
}

fn classify_connect_failure(error: WebSocketError) -> PrivateConnectFailure {
    match error {
        WebSocketError::Http(response) => PrivateConnectFailure::Http(response.status().as_u16()),
        WebSocketError::ConnectionClosed
        | WebSocketError::AlreadyClosed
        | WebSocketError::Io(_) => PrivateConnectFailure::Network,
        WebSocketError::Tls(_) => PrivateConnectFailure::Tls,
        WebSocketError::Capacity(_)
        | WebSocketError::Protocol(_)
        | WebSocketError::WriteBufferFull(_)
        | WebSocketError::Utf8(_)
        | WebSocketError::AttackAttempt
        | WebSocketError::Url(_)
        | WebSocketError::HttpFormat(_) => PrivateConnectFailure::Protocol,
    }
}

pub(super) fn is_client_handshake_header(name: &header::HeaderName) -> bool {
    *name == header::HOST
        || *name == header::CONNECTION
        || *name == header::UPGRADE
        || *name == header::CONTENT_LENGTH
        || *name == header::SEC_WEBSOCKET_KEY
        || *name == header::SEC_WEBSOCKET_VERSION
        || *name == header::SEC_WEBSOCKET_EXTENSIONS
        || *name == header::SEC_WEBSOCKET_PROTOCOL
        || name.as_str() == WS_TRACE_HEADER
}

pub(super) fn websocket_error_kind(error: &WebSocketError) -> &'static str {
    match error {
        WebSocketError::ConnectionClosed | WebSocketError::AlreadyClosed => "connection_closed",
        WebSocketError::Io(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => "eof",
        WebSocketError::Io(_) => "io",
        WebSocketError::Tls(_) => "tls",
        WebSocketError::Protocol(_) => "protocol",
        WebSocketError::Capacity(
            tokio_tungstenite::tungstenite::error::CapacityError::MessageTooLong { .. },
        ) => "message_limit",
        WebSocketError::Capacity(_) => "capacity",
        WebSocketError::Utf8(_) => "utf8",
        WebSocketError::WriteBufferFull(_) => "write_buffer_full",
        WebSocketError::AttackAttempt => "attack_attempt",
        WebSocketError::Url(_) => "url",
        WebSocketError::Http(_) => "http",
        WebSocketError::HttpFormat(_) => "http_format",
    }
}
