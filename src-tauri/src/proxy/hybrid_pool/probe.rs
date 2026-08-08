use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::{Bytes, Message};

use super::PrivateUpstream;

const KEEPALIVE_PAYLOAD: &[u8] = b"turbo-hybrid-pool";

pub(in crate::proxy) async fn probe_idle(
    mut upstream: PrivateUpstream,
    pong_timeout: Duration,
) -> Option<PrivateUpstream> {
    let payload = Bytes::from_static(KEEPALIVE_PAYLOAD);
    let healthy = tokio::time::timeout(pong_timeout, async {
        if upstream.send(Message::Ping(payload.clone())).await.is_err() {
            return false;
        }
        loop {
            match upstream.next().await {
                Some(Ok(Message::Pong(received))) if received == payload => return true,
                Some(Ok(Message::Pong(_))) => {}
                Some(Ok(Message::Ping(received))) => {
                    if upstream.send(Message::Pong(received)).await.is_err() {
                        return false;
                    }
                }
                Some(
                    Ok(
                        Message::Close(_)
                        | Message::Text(_)
                        | Message::Binary(_)
                        | Message::Frame(_),
                    )
                    | Err(_),
                )
                | None => return false,
            }
        }
    })
    .await
    .unwrap_or(false);
    healthy.then_some(upstream)
}
