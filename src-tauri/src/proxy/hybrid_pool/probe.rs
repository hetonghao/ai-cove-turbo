use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::{Bytes, Message};

use super::PrivateUpstream;

const KEEPALIVE_PAYLOAD: &[u8] = b"turbo-hybrid-pool";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::proxy) enum ProbeFailure {
    Timeout,
    Closed,
    WebSocket(&'static str),
    UnexpectedMessage,
}

impl ProbeFailure {
    pub(in crate::proxy) const fn reason(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::Closed => "connection_closed",
            Self::WebSocket(kind) => kind,
            Self::UnexpectedMessage => "unexpected_message",
        }
    }
}

pub(in crate::proxy) async fn probe_idle_detailed(
    upstream: &mut PrivateUpstream,
    pong_timeout: Duration,
) -> Result<(), ProbeFailure> {
    let payload = Bytes::from_static(KEEPALIVE_PAYLOAD);
    let result = tokio::time::timeout(pong_timeout, async {
        upstream
            .send(Message::Ping(payload.clone()))
            .await
            .map_err(|error| {
                ProbeFailure::WebSocket(super::super::private_websocket::websocket_error_kind(
                    &error,
                ))
            })?;
        loop {
            match upstream.next().await {
                Some(Ok(Message::Pong(received))) if received == payload => return Ok(()),
                Some(Ok(Message::Pong(_))) => {}
                Some(Ok(Message::Ping(received))) => {
                    upstream
                        .send(Message::Pong(received))
                        .await
                        .map_err(|error| {
                            ProbeFailure::WebSocket(
                                super::super::private_websocket::websocket_error_kind(&error),
                            )
                        })?;
                }
                Some(Ok(
                    Message::Close(_) | Message::Text(_) | Message::Binary(_) | Message::Frame(_),
                )) => return Err(ProbeFailure::UnexpectedMessage),
                Some(Err(error)) => {
                    return Err(ProbeFailure::WebSocket(
                        super::super::private_websocket::websocket_error_kind(&error),
                    ));
                }
                None => return Err(ProbeFailure::Closed),
            }
        }
    })
    .await;
    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(failure)) => Err(failure),
        Err(_) => Err(ProbeFailure::Timeout),
    }
}

pub(in crate::proxy) async fn probe_idle(
    mut upstream: PrivateUpstream,
    pong_timeout: Duration,
) -> Option<PrivateUpstream> {
    probe_idle_detailed(&mut upstream, pong_timeout)
        .await
        .ok()
        .map(|()| upstream)
}
