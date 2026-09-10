use std::io;

use serde_json::{Value, json};

use super::deepseek_history::{repair_deepseek_tool_history, tool_calls_from_event};

fn item_types(payload: &[u8]) -> io::Result<Vec<String>> {
    let value: Value = serde_json::from_slice(payload).map_err(io::Error::other)?;
    value
        .get("input")
        .and_then(Value::as_array)
        .ok_or_else(|| io::Error::other("input is missing"))
        .map(|input| {
            input
                .iter()
                .filter_map(|item| item.get("type").and_then(Value::as_str).map(str::to_owned))
                .collect()
        })
}

fn payload(model: &str, input: Value) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "model": model,
        "input": input,
    }))
    .expect("payload")
}

#[test]
fn deepseek_http_history_drops_unpaired_function_call() -> io::Result<()> {
    let body = payload(
        "deepseek-v4.1-flash",
        json!([
            {"type": "message", "role": "user", "content": "hi"},
            {"type": "function_call", "call_id": "call-1", "name": "exec_command", "arguments": "{}"}
        ]),
    );

    let repaired = repair_deepseek_tool_history(&body, &[])
        .ok_or_else(|| io::Error::other("expected DeepSeek repair"))?;
    assert_eq!(item_types(&repaired)?, vec!["message"]);
    Ok(())
}

#[test]
fn deepseek_http_history_keeps_paired_function_call() -> io::Result<()> {
    let body = payload(
        "deepseek-v4.1-flash",
        json!([
            {"type": "function_call", "call_id": "call-1", "name": "exec_command", "arguments": "{}"},
            {"type": "function_call_output", "call_id": "call-1", "output": "ok"}
        ]),
    );

    assert!(repair_deepseek_tool_history(&body, &[]).is_none());
    Ok(())
}

#[test]
fn deepseek_http_history_injects_cached_call_before_unpaired_output() -> io::Result<()> {
    let body = payload(
        "deepseek-v4.1-flash",
        json!([{"type": "function_call_output", "call_id": "call-1", "output": "ok"}]),
    );
    let cached = vec![json!({
        "type": "function_call",
        "call_id": "call-1",
        "name": "exec_command",
        "arguments": "{}"
    })];

    let repaired = repair_deepseek_tool_history(&body, &cached)
        .ok_or_else(|| io::Error::other("expected DeepSeek splice"))?;
    assert_eq!(
        item_types(&repaired)?,
        vec!["function_call", "function_call_output"]
    );
    Ok(())
}

#[test]
fn deepseek_http_history_does_not_invent_uncached_call() -> io::Result<()> {
    let body = payload(
        "deepseek-v4.1-flash",
        json!([{"type": "function_call_output", "call_id": "call-1", "output": "ok"}]),
    );

    assert!(repair_deepseek_tool_history(&body, &[]).is_none());
    Ok(())
}

#[test]
fn gemini_payload_is_left_untouched() {
    let body = payload(
        "gemini-3.8-flash",
        json!([{"type": "function_call", "call_id": "call-1", "name": "lookup"}]),
    );
    assert!(repair_deepseek_tool_history(&body, &[]).is_none());
}

#[test]
fn tool_calls_from_output_item_done_keep_call_id() {
    let event = serde_json::to_vec(&json!({
        "type": "response.output_item.done",
        "item": {
            "type": "function_call",
            "call_id": "call-1",
            "name": "exec_command",
            "arguments": "{\"cmd\":\"pwd\"}",
            "id": "fc_internal"
        }
    }))
    .expect("event");

    let calls = tool_calls_from_event(&event);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["type"], "function_call");
    assert_eq!(calls[0]["call_id"], "call-1");
    assert_eq!(calls[0]["name"], "exec_command");
    assert_eq!(calls[0]["arguments"], "{\"cmd\":\"pwd\"}");
    assert!(calls[0].get("id").is_none());
}
