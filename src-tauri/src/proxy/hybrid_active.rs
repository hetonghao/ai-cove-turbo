use futures_util::SinkExt;
use tokio_tungstenite::tungstenite::{Error as WebSocketError, Message, error::ProtocolError};

use super::super::hybrid_pool::LeaseRetirement;
use super::common::close_client;
use super::sse::{
    idle_event_diagnostic, is_internal_idle_request_error, is_success_terminal_event,
    success_terminal_response_id,
};
use super::{ClientWebSocket, Session};

pub(super) async fn handle_idle_upstream(
    client: &mut ClientWebSocket,
    session: &mut Session,
    message: Option<Result<Message, WebSocketError>>,
) -> bool {
    let message = match resolve_idle_upstream_message(client, session, message).await {
        Ok(message) => message,
        Err(keep_running) => return keep_running,
    };
    match message {
        Message::Ping(payload) => {
            let Some(ready) = session.ready.as_mut() else {
                return false;
            };
            let Some(upstream) = ready.upstream_mut() else {
                return false;
            };
            upstream.send(Message::Pong(payload)).await.is_ok()
        }
        Message::Pong(_) => true,
        Message::Close(frame) => {
            let code = frame.as_ref().map_or(1000, |frame| u16::from(frame.code));
            let reason = frame
                .as_ref()
                .map_or("upstream closed", |frame| frame.reason.as_ref());
            session.state.metrics.record_websocket_diagnostic(
                &session.path,
                code,
                crate::proxy::traffic::FailurePhase::HybridIdle,
                reason,
            );
            let retirement = LeaseRetirement::Recovering {
                reason: format!("上游关闭 · {code} · {reason}"),
            };
            session.state.metrics.record_websocket_closed();
            session.discard(retirement).await;
            if code == 1012 {
                session.drain_reconnect_pending = true;
            }
            if matches!(code, 1011 | 1012) {
                return true;
            }
            let _ = close_client(client, code, "idle upstream websocket closed").await;
            false
        }
        Message::Text(_) => {
            recover_unexpected_idle_message(session, "空闲上游 WebSocket 收到意外文本消息").await
        }
        Message::Binary(payload) => {
            let Ok(decoded) = super::private_websocket::decode_private_message_async(payload).await
            else {
                return recover_unexpected_idle_message(
                    session,
                    "空闲上游 WebSocket 收到意外二进制消息；解码=私有帧失败；事件=未知；响应ID=未知",
                )
                .await;
            };
            let response_id = success_terminal_response_id(&decoded.payload);
            let reusable = is_success_terminal_event(&decoded.payload)
                && response_id.is_some()
                && response_id.as_ref() == session.last_terminal_response_id.as_ref();
            if reusable {
                true
            } else if is_internal_idle_request_error(&decoded.payload) {
                session
                    .retire_idle_upstream(LeaseRetirement::Replacing)
                    .await;
                true
            } else {
                let reason = idle_event_diagnostic(
                    &decoded.payload,
                    session.last_terminal_response_id.as_deref(),
                );
                recover_unexpected_idle_message(session, &reason).await
            }
        }
        Message::Frame(_) => {
            recover_unexpected_idle_message(session, "空闲上游 WebSocket 收到意外原始帧").await
        }
    }
}

async fn resolve_idle_upstream_message(
    client: &mut ClientWebSocket,
    session: &mut Session,
    message: Option<Result<Message, WebSocketError>>,
) -> Result<Message, bool> {
    let Some(message) = message else {
        session.state.metrics.record_websocket_diagnostic(
            &session.path,
            1011,
            crate::proxy::traffic::FailurePhase::HybridIdle,
            "upstream stream ended",
        );
        session.state.metrics.record_websocket_closed();
        session
            .discard(LeaseRetirement::Recovering {
                reason: "上游连接已结束".to_owned(),
            })
            .await;
        return Err(true);
    };
    let Err(error) = message else {
        return message.map_err(|_| false);
    };
    let reason = error.to_string();
    let code = super::private_websocket::websocket_error_code(&error);
    session.state.metrics.record_websocket_diagnostic(
        &session.path,
        code,
        crate::proxy::traffic::FailurePhase::HybridIdle,
        &reason,
    );
    session.state.metrics.record_websocket_closed();
    let recoverable = matches!(
        error,
        WebSocketError::ConnectionClosed
            | WebSocketError::Io(_)
            | WebSocketError::Tls(_)
            | WebSocketError::Protocol(ProtocolError::ResetWithoutClosingHandshake)
    );
    session
        .discard(LeaseRetirement::Recovering {
            reason: reason.clone(),
        })
        .await;
    if recoverable {
        return Err(true);
    }
    let _ = close_client(client, code, "idle upstream websocket failed").await;
    Err(false)
}

async fn recover_unexpected_idle_message(session: &mut Session, reason: &str) -> bool {
    session.state.metrics.record_websocket_diagnostic(
        &session.path,
        1002,
        crate::proxy::traffic::FailurePhase::HybridIdle,
        reason,
    );
    session
        .retire_idle_upstream(LeaseRetirement::Recovering {
            reason: reason.to_owned(),
        })
        .await;
    true
}

pub(super) async fn handle_idle_keepalive(session: &mut Session) -> bool {
    let Some(mut lease) = session.ready.take() else {
        return true;
    };
    let Some(upstream) = lease.take_upstream() else {
        return true;
    };
    if let Some(upstream) =
        super::super::hybrid_pool::probe_idle(upstream, super::super::hybrid_pool::PONG_TIMEOUT)
            .await
    {
        lease.put_upstream(upstream);
        session.ready = Some(lease);
        return true;
    }
    session.state.metrics.record_websocket_diagnostic(
        &session.path,
        1011,
        crate::proxy::traffic::FailurePhase::HybridIdle,
        "private websocket keepalive failed",
    );
    session.state.metrics.record_websocket_closed();
    session.ready = Some(lease);
    session
        .discard(LeaseRetirement::Recovering {
            reason: "WebSocket 健康检查未通过".to_owned(),
        })
        .await;
    true
}
