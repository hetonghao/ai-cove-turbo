use tokio_tungstenite::tungstenite::Message;

use super::super::hybrid_pool::LeaseRetirement;
use super::common::{close_client, send_error};
use super::worker::retire_failed_websocket;
use super::{Active, ActiveKind, ClientWebSocket, Session, TransportFallback, http};
use crate::proxy::HttpTraffic;

pub(super) enum Action {
    Forward(Message),
    StartedHttp,
    Stop,
}

pub(super) async fn handle_stopped_worker(client: &mut ClientWebSocket) -> bool {
    let message = "response worker stopped unexpectedly";
    let _ = send_error(client, "server_error", message).await;
    let _ = close_client(client, 1011, message).await;
    false
}

pub(super) async fn apply(
    session: &mut Session,
    active: &mut Option<Active>,
    fallback: TransportFallback,
) -> Action {
    let TransportFallback {
        response,
        code,
        reason,
    } = fallback;
    let http_payload = active.as_mut().and_then(|item| {
        (item.kind == ActiveKind::WebSocket && !item.output_forwarded && !item.cancel_requested)
            .then(|| item.http_fallback.take())
            .flatten()
    });
    let Some(http_payload) = http_payload else {
        return if retire_failed_websocket(session, active, code, &reason).await {
            Action::Forward(response)
        } else {
            Action::Stop
        };
    };
    active.take();
    session.websocket_receipt.take();
    session
        .discard(LeaseRetirement::Recovering { reason })
        .await;
    *active = Some(http::start_http_worker(
        session,
        http_payload,
        HttpTraffic::HYBRID_RECOVERY,
    ));
    Action::StartedHttp
}
