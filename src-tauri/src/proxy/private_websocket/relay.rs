use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use tokio::task::JoinError;
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{
        Bytes, Utf8Bytes,
        protocol::{CloseFrame, Message, Role, WebSocketConfig, frame::coding::CloseCode},
    },
};

use super::{
    PrivateUpstream,
    codec::{
        DecodedPrivateMessage, FLAG_ZSTD_COMPRESSED, PRIVATE_MESSAGE_MAX_BYTES,
        PrivateProtocolError, decode_private_message, encode_private_message,
    },
};
use crate::proxy::Metrics;

pub(in crate::proxy) async fn relay_private(
    upgraded: Upgraded,
    mut upstream: PrivateUpstream,
    metrics: Arc<Metrics>,
    path: String,
) {
    let config = WebSocketConfig::default()
        .max_message_size(Some(PRIVATE_MESSAGE_MAX_BYTES))
        .max_frame_size(Some(PRIVATE_MESSAGE_MAX_BYTES));
    let mut client =
        WebSocketStream::from_raw_socket(TokioIo::new(upgraded), Role::Server, Some(config)).await;

    loop {
        tokio::select! {
            client_message = client.next() => {
                let Some(client_message) = client_message else {
                    let _ = upstream.close(None).await;
                    break;
                };
                let message = match client_message {
                    Ok(message) => message,
                    Err(error) => {
                        let close_code = websocket_error_code(&error);
                        metrics.record_websocket_error(&path, close_code);
                        close_both(&mut client, &mut upstream, close_code, "client websocket error").await;
                        break;
                    }
                };
                if !forward_client_message(&mut client, &mut upstream, message, &metrics, &path).await {
                    break;
                }
            }
            upstream_message = upstream.next() => {
                let Some(upstream_message) = upstream_message else {
                    let _ = client.close(None).await;
                    break;
                };
                let message = match upstream_message {
                    Ok(message) => message,
                    Err(error) => {
                        let close_code = websocket_error_code(&error);
                        metrics.record_websocket_error(&path, close_code);
                        close_both(&mut client, &mut upstream, close_code, "upstream websocket error").await;
                        break;
                    }
                };
                if !forward_upstream_message(&mut client, &mut upstream, message, &metrics, &path).await {
                    break;
                }
            }
        }
    }
}

async fn forward_client_message(
    client: &mut WebSocketStream<TokioIo<Upgraded>>,
    upstream: &mut PrivateUpstream,
    message: Message,
    metrics: &Metrics,
    path: &str,
) -> bool {
    match message {
        Message::Text(text) => {
            forward_application_or_close(
                client,
                upstream,
                text.as_bytes().to_vec(),
                false,
                metrics,
                path,
            )
            .await
        }
        Message::Binary(payload) => {
            forward_application_or_close(client, upstream, payload.to_vec(), true, metrics, path)
                .await
        }
        Message::Ping(_) | Message::Pong(_) => true,
        Message::Close(frame) => {
            let _ = upstream.send(Message::Close(frame)).await;
            false
        }
        Message::Frame(_) => {
            metrics.record_websocket_error(path, 1002);
            close_both(client, upstream, 1002, "raw websocket frame is invalid").await;
            false
        }
    }
}

async fn forward_application_or_close(
    client: &mut WebSocketStream<TokioIo<Upgraded>>,
    upstream: &mut PrivateUpstream,
    payload: Vec<u8>,
    original_binary: bool,
    metrics: &Metrics,
    path: &str,
) -> bool {
    match forward_private_application(upstream, payload, original_binary, metrics, path).await {
        Ok(()) => true,
        Err(error) => {
            metrics.record_websocket_error(path, error.close_code);
            close_both(client, upstream, error.close_code, &error.to_string()).await;
            false
        }
    }
}

async fn forward_upstream_message(
    client: &mut WebSocketStream<TokioIo<Upgraded>>,
    upstream: &mut PrivateUpstream,
    message: Message,
    metrics: &Metrics,
    path: &str,
) -> bool {
    match message {
        Message::Binary(envelope) => match decode_private_message_async(envelope).await {
            Ok(decoded) => match decoded_message(decoded) {
                Ok(message) => {
                    if client.send(message).await.is_ok() {
                        true
                    } else {
                        metrics.record_websocket_error(path, 1011);
                        false
                    }
                }
                Err(error) => {
                    metrics.record_websocket_error(path, error.close_code);
                    close_both(client, upstream, error.close_code, &error.to_string()).await;
                    false
                }
            },
            Err(error) => {
                metrics.record_websocket_error(path, error.close_code);
                close_both(client, upstream, error.close_code, &error.to_string()).await;
                false
            }
        },
        Message::Text(_) | Message::Frame(_) => {
            metrics.record_websocket_error(path, 1002);
            close_both(
                client,
                upstream,
                1002,
                "private application frame must be binary",
            )
            .await;
            false
        }
        Message::Ping(_) | Message::Pong(_) => true,
        Message::Close(frame) => {
            let _ = client.send(Message::Close(frame)).await;
            false
        }
    }
}

async fn forward_private_application(
    upstream: &mut PrivateUpstream,
    payload: Vec<u8>,
    original_binary: bool,
    metrics: &Metrics,
    path: &str,
) -> Result<(), PrivateProtocolError> {
    let raw_len = payload.len();
    let encoded = encode_private_message_async(payload, original_binary).await?;
    let compressed = encoded
        .get(5)
        .is_some_and(|flags| flags & FLAG_ZSTD_COMPRESSED != 0);
    let sent_len = encoded.len();
    upstream
        .send(Message::Binary(Bytes::from(encoded)))
        .await
        .map_err(|_| PrivateProtocolError::internal("private websocket send failed"))?;
    metrics.record_websocket_zstd_message(path, raw_len, sent_len, compressed);
    Ok(())
}

const fn should_offload_private_encoding(payload_len: usize) -> bool {
    payload_len >= super::super::MIN_COMPRESSION_INPUT_BYTES
}

pub(in crate::proxy) async fn encode_private_message_async(
    payload: Vec<u8>,
    original_binary: bool,
) -> Result<Vec<u8>, PrivateProtocolError> {
    if should_offload_private_encoding(payload.len()) {
        return tokio::task::spawn_blocking(move || {
            encode_private_message(&payload, original_binary)
        })
        .await
        .map_err(join_error)?;
    }
    encode_private_message(&payload, original_binary)
}

async fn decode_private_message_async(
    envelope: Bytes,
) -> Result<DecodedPrivateMessage, PrivateProtocolError> {
    tokio::task::spawn_blocking(move || decode_private_message(&envelope))
        .await
        .map_err(join_error)?
}

fn decoded_message(decoded: DecodedPrivateMessage) -> Result<Message, PrivateProtocolError> {
    if decoded.original_binary {
        return Ok(Message::Binary(Bytes::from(decoded.payload)));
    }
    Utf8Bytes::try_from(decoded.payload)
        .map(Message::Text)
        .map_err(|_| PrivateProtocolError::protocol("private text is not UTF-8"))
}

fn join_error(_: JoinError) -> PrivateProtocolError {
    PrivateProtocolError::internal("private websocket worker failed")
}

async fn close_both(
    client: &mut WebSocketStream<TokioIo<Upgraded>>,
    upstream: &mut PrivateUpstream,
    close_code: u16,
    reason: &str,
) {
    let frame = CloseFrame {
        code: CloseCode::from(close_code),
        reason: reason.to_owned().into(),
    };
    let _ = client.send(Message::Close(Some(frame.clone()))).await;
    let _ = upstream.send(Message::Close(Some(frame))).await;
}

const fn websocket_error_code(error: &tokio_tungstenite::tungstenite::Error) -> u16 {
    match error {
        tokio_tungstenite::tungstenite::Error::Capacity(_) => 1009,
        tokio_tungstenite::tungstenite::Error::Utf8(_) => 1007,
        tokio_tungstenite::tungstenite::Error::Protocol(_) => 1002,
        _ => 1011,
    }
}

#[cfg(test)]
mod tests;
