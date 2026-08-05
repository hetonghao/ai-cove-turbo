use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{
        Bytes, Utf8Bytes,
        protocol::{CloseFrame, Message, frame::coding::CloseCode},
    },
};

use super::{
    PrivateUpstream,
    codec::{DecodedPrivateMessage, PrivateProtocolError},
    decode_private_message_async, encode_private_message_async,
};
use crate::proxy::Metrics;

#[cfg(test)]
pub(super) use super::should_offload_private_encoding;

pub(in crate::proxy) async fn relay_private_from_message(
    client: &mut WebSocketStream<TokioIo<Upgraded>>,
    upstream: &mut PrivateUpstream,
    initial: Message,
    metrics: Arc<Metrics>,
    path: String,
) {
    if forward_client_message(client, upstream, initial, &metrics, &path).await {
        relay_private_refs(client, upstream, metrics, path).await;
    }
}

async fn relay_private_refs(
    client: &mut WebSocketStream<TokioIo<Upgraded>>,
    upstream: &mut PrivateUpstream,
    metrics: Arc<Metrics>,
    path: String,
) {
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
                        close_both(client, upstream, close_code, "client websocket error").await;
                        break;
                    }
                };
                if !forward_client_message(client, upstream, message, &metrics, &path).await {
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
                        close_both(client, upstream, close_code, "upstream websocket error").await;
                        break;
                    }
                };
                if !forward_upstream_message(client, upstream, message, &metrics, &path).await {
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
    let sent_len = encoded.bytes.len();
    upstream
        .send(Message::Binary(Bytes::from(encoded.bytes)))
        .await
        .map_err(|_| PrivateProtocolError::internal("private websocket send failed"))?;
    metrics.record_websocket_zstd_message(path, raw_len, sent_len, encoded.compressed);
    Ok(())
}

fn decoded_message(decoded: DecodedPrivateMessage) -> Result<Message, PrivateProtocolError> {
    if decoded.original_binary {
        return Ok(Message::Binary(Bytes::from(decoded.payload)));
    }
    Utf8Bytes::try_from(decoded.payload)
        .map(Message::Text)
        .map_err(|_| PrivateProtocolError::protocol("private text is not UTF-8"))
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
