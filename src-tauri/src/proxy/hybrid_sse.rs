use std::mem;

use serde_json::Value;

pub(super) enum HttpFallback {
    Request(Vec<u8>),
    WebSocketRequired,
}

pub(super) fn http_request_payload(payload: &[u8]) -> Result<HttpFallback, String> {
    let mut value: Value = serde_json::from_slice(payload).map_err(|error| error.to_string())?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| "response.create must be a JSON object".to_owned())?;
    if object
        .get("previous_response_id")
        .and_then(Value::as_str)
        .is_some_and(|response_id| !response_id.is_empty())
    {
        return Ok(HttpFallback::WebSocketRequired);
    }
    object.remove("type");
    object.insert("stream".to_owned(), Value::Bool(true));
    serde_json::to_vec(&value)
        .map(HttpFallback::Request)
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
    if payload == b"[DONE]" {
        return true;
    }
    serde_json::from_slice::<Value>(payload)
        .ok()
        .and_then(|value| value.get("type").and_then(Value::as_str).map(str::to_owned))
        .is_some_and(|event_type| {
            matches!(
                event_type.as_str(),
                "response.completed"
                    | "response.done"
                    | "response.failed"
                    | "response.incomplete"
                    | "response.cancelled"
                    | "response.canceled"
                    | "error"
            )
        })
}
