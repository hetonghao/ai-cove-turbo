use std::mem;

use serde_json::Value;

pub(super) enum HttpFallback {
    Request(Vec<u8>),
    WebSocketRequired,
}

pub(super) struct PreparedResponseCreate {
    pub(super) fallback: HttpFallback,
    pub(super) thread_id: Option<String>,
    pub(super) previous_response_id: Option<String>,
}

pub(super) fn http_request_payload(payload: &[u8]) -> Result<PreparedResponseCreate, String> {
    let mut value: Value = serde_json::from_slice(payload).map_err(|error| error.to_string())?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| "response.create must be a JSON object".to_owned())?;
    let thread_id = object
        .get("client_metadata")
        .and_then(Value::as_object)
        .and_then(|metadata| {
            metadata
                .get("x-codex-turn-metadata")
                .and_then(Value::as_str)
                .and_then(|metadata| serde_json::from_str::<Value>(metadata).ok())
                .and_then(|metadata| {
                    metadata
                        .get("thread_id")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .or_else(|| {
                    metadata
                        .get("thread_id")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
        });
    let previous_response_id = object
        .get("previous_response_id")
        .and_then(Value::as_str)
        .filter(|response_id| !response_id.is_empty())
        .map(str::to_owned);
    if previous_response_id.is_some() {
        return Ok(PreparedResponseCreate {
            fallback: HttpFallback::WebSocketRequired,
            thread_id,
            previous_response_id,
        });
    }
    object.remove("type");
    object.insert("stream".to_owned(), Value::Bool(true));
    serde_json::to_vec(&value)
        .map(|payload| PreparedResponseCreate {
            fallback: HttpFallback::Request(payload),
            thread_id,
            previous_response_id: None,
        })
        .map_err(|error| error.to_string())
}

#[derive(Debug, Default)]
pub(super) struct SseParser {
    pending: Vec<u8>,
    data: Vec<u8>,
    pub(super) events: Vec<Vec<u8>>,
}

impl SseParser {
    pub(super) fn push(&mut self, chunk: &[u8]) {
        self.pending.extend_from_slice(chunk);
        while let Some(newline) = self.pending.iter().position(|byte| *byte == b'\n') {
            let mut line = self.pending.drain(..=newline).collect::<Vec<_>>();
            let _ = line.pop();
            if line.last() == Some(&b'\r') {
                let _ = line.pop();
            }
            self.push_line(&line);
        }
    }

    fn push_line(&mut self, line: &[u8]) {
        if line.is_empty() {
            self.finish_event();
            return;
        }
        let Some(data) = line.strip_prefix(b"data:") else {
            return;
        };
        let data = data.strip_prefix(b" ").unwrap_or(data);
        if !self.data.is_empty() {
            self.data.push(b'\n');
        }
        self.data.extend_from_slice(data);
    }

    fn finish_event(&mut self) {
        if self.data.is_empty() {
            return;
        }
        self.events.push(mem::take(&mut self.data));
    }

    pub(super) fn take_events(&mut self) -> Vec<Vec<u8>> {
        mem::take(&mut self.events)
    }

    pub(super) fn from_events(events: Vec<Vec<u8>>) -> Self {
        Self {
            events,
            ..Self::default()
        }
    }

    pub(super) fn finish(&mut self) -> Vec<Vec<u8>> {
        if !self.pending.is_empty() {
            let line = mem::take(&mut self.pending);
            self.push_line(line.strip_suffix(b"\r").unwrap_or(&line));
        }
        self.finish_event();
        mem::take(&mut self.events)
    }
}

pub(super) fn is_terminal_event(payload: &[u8]) -> bool {
    if payload == b"[DONE]" || is_success_terminal_event(payload) {
        return true;
    }
    serde_json::from_slice::<Value>(payload)
        .ok()
        .and_then(|value| value.get("type").and_then(Value::as_str).map(str::to_owned))
        .is_some_and(|event_type| {
            matches!(
                event_type.as_str(),
                "response.failed"
                    | "response.incomplete"
                    | "response.cancelled"
                    | "response.canceled"
                    | "error"
            )
        })
}

pub(super) fn is_success_terminal_event(payload: &[u8]) -> bool {
    serde_json::from_slice::<Value>(payload)
        .ok()
        .and_then(|value| value.get("type").and_then(Value::as_str).map(str::to_owned))
        .is_some_and(|event_type| {
            matches!(event_type.as_str(), "response.completed" | "response.done")
        })
}

pub(super) fn success_terminal_response_id(payload: &[u8]) -> Option<String> {
    let value: Value = serde_json::from_slice(payload).ok()?;
    if !matches!(
        value.get("type").and_then(Value::as_str),
        Some("response.completed" | "response.done")
    ) {
        return None;
    }
    value
        .pointer("/response/id")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

fn diagnostic_token(value: Option<&str>) -> Option<&str> {
    value.filter(|value| {
        !value.is_empty()
            && value.len() <= 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    })
}

pub(super) fn is_internal_idle_request_error(payload: &[u8]) -> bool {
    let Ok(value) = serde_json::from_slice::<Value>(payload) else {
        return false;
    };
    value.get("type").and_then(Value::as_str) == Some("error")
        && value.pointer("/response/id").is_none()
        && value.get("response_id").is_none()
        && value.pointer("/error/code").and_then(Value::as_str) == Some("do_request_failed")
}

pub(super) fn idle_event_diagnostic(payload: &[u8], last_response_id: Option<&str>) -> String {
    let Ok(value) = serde_json::from_slice::<Value>(payload) else {
        return "空闲上游 WebSocket 收到意外二进制消息；解码=JSON失败；事件=未知；响应ID=未知"
            .to_owned();
    };
    let event_type = diagnostic_token(value.get("type").and_then(Value::as_str)).unwrap_or("未知");
    let response_id = value
        .pointer("/response/id")
        .or_else(|| value.get("response_id"))
        .and_then(Value::as_str);
    let response_id_relation = match (response_id, last_response_id) {
        (None, _) => "缺失",
        (Some(_), None) => "无历史终态",
        (Some(current), Some(previous)) if current == previous => "匹配",
        (Some(_), Some(_)) => "不匹配",
    };
    let error_code = (event_type == "error")
        .then(|| diagnostic_token(value.pointer("/error/code").and_then(Value::as_str)))
        .flatten();
    let error_detail = error_code.map_or_else(String::new, |code| format!("；错误码={code}"));
    format!(
        "空闲上游 WebSocket 收到意外二进制消息；解码=成功；事件={event_type}；响应ID={response_id_relation}{error_detail}"
    )
}
