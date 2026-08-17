use futures_util::SinkExt;
use serde_json::Value;
use tokio_tungstenite::tungstenite::{
    Utf8Bytes,
    protocol::{CloseFrame, Message, frame::coding::CloseCode},
};

use super::ClientWebSocket;

pub(super) fn event_type(payload: &[u8]) -> Result<String, String> {
    let value: Value = serde_json::from_slice(payload).map_err(|error| error.to_string())?;
    value
        .get("type")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| "event type is required".to_owned())
}

pub(super) fn text_message(payload: Vec<u8>) -> Result<Message, ()> {
    Utf8Bytes::try_from(payload)
        .map(Message::Text)
        .map_err(|_| ())
}

pub(super) fn error_message(code: &str, message: &str) -> Message {
    let payload = serde_json::json!({
        "type": "error",
        "error": {"code": code, "message": message},
    });
    Message::Text(payload.to_string().into())
}

pub(super) fn context_length_exceeded_message() -> Message {
    let payload = serde_json::json!({
        "type": "response.failed",
        "response": {
            "status": "failed",
            "error": {
                "code": "context_length_exceeded",
                "message": "Your input exceeds the supported context size. Please reduce it and try again."
            }
        }
    });
    Message::Text(payload.to_string().into())
}

pub(super) async fn send_error(client: &mut ClientWebSocket, code: &str, message: &str) -> bool {
    client.send(error_message(code, message)).await.is_ok()
}

pub(super) async fn close_client(client: &mut ClientWebSocket, code: u16, reason: &str) -> bool {
    client
        .send(Message::Close(Some(CloseFrame {
            code: CloseCode::from(code),
            reason: reason.to_owned().into(),
        })))
        .await
        .is_ok()
}

pub(super) async fn reject_thread_switch(client: &mut ClientWebSocket) -> bool {
    let message = "同一 WebSocket 不能切换 Codex 会话";
    let _ = send_error(client, "invalid_request", message).await;
    let _ = close_client(client, 1002, message).await;
    false
}
