use std::io;

use serde_json::{Value, json};

use super::continuation_merge::{PendingHttpContinuation, expand, merge_input};

fn previous_request() -> Value {
    json!({
        "model": "gpt-6-astra",
        "instructions": "be brief",
        "prompt_cache_key": "thread-1",
        "stream": true,
        "input": [
            {"type": "message", "role": "user", "id": "msg-1", "content": "画画"},
        ],
    })
}

fn previous_output() -> Vec<Value> {
    vec![
        json!({"type": "reasoning", "id": "rs-1", "summary": []}),
        json!({
            "type": "custom_tool_call",
            "id": "ctc-1",
            "call_id": "call-1",
            "name": "exec",
            "input": "echo ok",
        }),
    ]
}

fn continuation(input: &Value) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "type": "response.create",
        "model": "gpt-6-astra",
        "previous_response_id": "resp-1",
        "input": input,
    }))
    .unwrap_or_default()
}

fn item_types(expanded: &Value) -> Vec<String> {
    expanded
        .get("input")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("type").and_then(Value::as_str).map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn expand_pairs_incremental_tool_output_with_previous_response_call() -> io::Result<()> {
    // Given: 上一轮响应产出 custom_tool_call，客户端只回带它的输出。
    let payload = continuation(&json!([
        {"type": "custom_tool_call_output", "call_id": "call-1", "output": "done"},
    ]));

    // When: 展开成自包含请求。
    let expanded: Value = serde_json::from_slice(
        &expand(&previous_request(), &previous_output(), &payload)
            .ok_or_else(|| io::Error::other("expansion failed"))?,
    )
    .map_err(io::Error::other)?;

    // Then: tool call 与它的输出成对出现，且请求不再依赖服务端状态。
    assert_eq!(
        item_types(&expanded),
        vec![
            "message",
            "reasoning",
            "custom_tool_call",
            "custom_tool_call_output"
        ]
    );
    assert_eq!(expanded.pointer("/input/2/call_id"), Some(&json!("call-1")));
    assert_eq!(expanded.pointer("/input/3/call_id"), Some(&json!("call-1")));
    assert!(expanded.get("previous_response_id").is_none());
    assert!(expanded.get("type").is_none());
    assert_eq!(expanded.get("stream"), Some(&Value::Bool(true)));
    Ok(())
}

#[test]
fn expand_keeps_previous_request_fields_and_overlays_delta_fields() -> io::Result<()> {
    // Given: 增量帧只带少数字段。
    let payload = continuation(&json!([{"type": "message", "role": "user", "content": "继续"}]));

    // When: 展开。
    let expanded: Value = serde_json::from_slice(
        &expand(&previous_request(), &previous_output(), &payload)
            .ok_or_else(|| io::Error::other("expansion failed"))?,
    )
    .map_err(io::Error::other)?;

    // Then: 上游需要的模型级字段来自上一轮请求，增量字段覆盖同名历史字段。
    assert_eq!(expanded.get("model"), Some(&json!("gpt-6-astra")));
    assert_eq!(expanded.get("instructions"), Some(&json!("be brief")));
    assert_eq!(expanded.get("prompt_cache_key"), Some(&json!("thread-1")));
    Ok(())
}

#[test]
fn expand_rejects_payloads_it_cannot_merge() {
    // 字符串 input：不是可拼接的条目列表。
    assert!(
        expand(
            &previous_request(),
            &previous_output(),
            &continuation(&json!("next"))
        )
        .is_none()
    );
    // 上一轮底稿缺少 input：无从重建 transcript。
    let broken = json!({"model": "gpt-6-astra"});
    assert!(expand(&broken, &previous_output(), &continuation(&json!([]))).is_none());
    // 非 JSON 帧。
    assert!(expand(&previous_request(), &previous_output(), b"not json").is_none());
}

#[test]
fn expand_refuses_unpaired_tool_output() {
    // Given: 上一轮响应没有带回那条 tool call（例如响应流缺 output items）。
    let payload = continuation(&json!([
        {"type": "custom_tool_call_output", "call_id": "call-1", "output": "done"},
    ]));

    // When / Then: 不做可能被上游拒绝的展开，交给本地兜底。
    assert!(expand(&previous_request(), &[], &payload).is_none());
}

#[test]
fn merge_dedupes_repeated_ids_keeping_delta_copy() {
    // Given: 增量帧重复回带了历史上同 id 的 message。
    let previous = vec![json!({"type": "message", "id": "msg-1", "content": "旧"})];
    let delta = vec![json!({"type": "message", "id": "msg-1", "content": "新"})];

    // When: 合并。
    let merged = merge_input(&previous, &[], &delta);

    // Then: 只保留一份，且以增量副本为准。
    assert_eq!(merged.len(), 1);
    assert_eq!(
        merged.first().and_then(|item| item.get("content")),
        Some(&json!("新"))
    );
}

#[test]
fn merge_dedupes_repeated_tool_calls_keeping_first() {
    // Given: 上一轮请求与响应里出现同一个 call_id 的 tool call。
    let previous = vec![json!({"type": "custom_tool_call", "id": "ctc-1", "call_id": "call-1"})];
    let output = vec![json!({"type": "custom_tool_call", "id": "ctc-1b", "call_id": "call-1"})];

    // When: 合并。
    let merged = merge_input(&previous, &output, &[]);

    // Then: 调用只留第一次出现的那份。
    assert_eq!(merged.len(), 1);
    assert_eq!(
        merged.first().and_then(|item| item.get("id")),
        Some(&json!("ctc-1"))
    );
}

#[test]
fn pending_continuation_without_request_yields_nothing() {
    let mut pending = PendingHttpContinuation::new(b"not json");
    pending.observe(br#"{"type":"response.output_item.done","output_index":0,"item":{"type":"message","id":"msg-1"}}"#);
    assert!(pending.finish().is_none());
}

#[test]
fn pending_continuation_prefers_terminal_output() -> io::Result<()> {
    // Given: 增量事件先于终态事件到达。
    let mut pending = PendingHttpContinuation::new(br#"{"model":"test","input":[]}"#);
    pending.observe(br#"{"type":"response.output_item.done","output_index":0,"item":{"type":"message","id":"msg-1"}}"#);
    pending.observe(br#"{"type":"response.completed","response":{"id":"resp-1","output":[{"type":"message","id":"msg-1"},{"type":"custom_tool_call","id":"ctc-1","call_id":"call-1"}]}}"#);

    // When: 固化。
    let finished = pending
        .finish()
        .ok_or_else(|| io::Error::other("continuation missing"))?;
    let expanded: Value = serde_json::from_slice(
        &finished
            .expand(&continuation(&json!([{
                "type": "custom_tool_call_output",
                "call_id": "call-1",
            }])))
            .ok_or_else(|| io::Error::other("expansion failed"))?,
    )
    .map_err(io::Error::other)?;

    // Then: 使用终态清单，且顺序按 output_index。
    assert_eq!(
        item_types(&expanded),
        vec!["message", "custom_tool_call", "custom_tool_call_output"]
    );
    Ok(())
}

#[test]
fn pending_continuation_orders_indexed_items_before_unindexed() -> io::Result<()> {
    let mut pending = PendingHttpContinuation::new(br#"{"model":"test","input":[]}"#);
    pending.observe(br#"{"type":"response.output_item.done","output_index":1,"item":{"type":"message","id":"msg-2"}}"#);
    pending
        .observe(br#"{"type":"response.output_item.done","item":{"type":"message","id":"msg-1"}}"#);
    pending.observe(br#"{"type":"response.output_item.done","output_index":0,"item":{"type":"message","id":"msg-0"}}"#);

    let finished = pending
        .finish()
        .ok_or_else(|| io::Error::other("continuation missing"))?;
    let expanded: Value = serde_json::from_slice(
        &finished
            .expand(&continuation(&json!([{"type": "message", "id": "msg-9"}])))
            .ok_or_else(|| io::Error::other("expansion failed"))?,
    )
    .map_err(io::Error::other)?;

    let ids: Vec<String> = expanded
        .get("input")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("id").and_then(Value::as_str).map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(ids, vec!["msg-0", "msg-2", "msg-1", "msg-9"]);
    Ok(())
}
