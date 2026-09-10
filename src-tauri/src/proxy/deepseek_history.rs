use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value};

pub(super) fn repair_deepseek_tool_history(
    payload: &[u8],
    cached_calls: &[Value],
) -> Option<Vec<u8>> {
    let mut request: Value = serde_json::from_slice(payload).ok()?;
    let is_deepseek = request
        .get("model")
        .and_then(Value::as_str)
        .is_some_and(|model| model.starts_with("deepseek"));
    if !is_deepseek {
        return None;
    }
    let input = request.get_mut("input").and_then(Value::as_array_mut)?;
    if !repair_input(input, cached_calls) {
        return None;
    }
    serde_json::to_vec(&request).ok()
}

pub(super) fn tool_calls_from_event(payload: &[u8]) -> Vec<Value> {
    let Ok(value) = serde_json::from_slice::<Value>(payload) else {
        return Vec::new();
    };
    match value.get("type").and_then(Value::as_str) {
        Some("response.output_item.done") => value
            .get("item")
            .and_then(input_call_item)
            .into_iter()
            .collect(),
        Some("response.completed" | "response.done") => value
            .pointer("/response/output")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(input_call_item)
            .collect(),
        _ => Vec::new(),
    }
}

fn repair_input(input: &mut Vec<Value>, cached_calls: &[Value]) -> bool {
    let output_ids = call_ids(input, &["function_call_output", "custom_tool_call_output"]);
    let mut present_call_ids = call_ids(input, &["function_call", "custom_tool_call"]);
    let cache_by_id = cached_call_map(cached_calls);
    let source = input.clone();
    let mut repaired = Vec::with_capacity(source.len() + cache_by_id.len());
    let mut changed = false;
    for item in source {
        let item_type = item
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let call_id = item
            .get("call_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if matches!(item_type.as_str(), "function_call" | "custom_tool_call") {
            if call_id.is_empty() || !output_ids.contains(call_id.as_str()) {
                changed = true;
                continue;
            }
        } else if matches!(
            item_type.as_str(),
            "function_call_output" | "custom_tool_call_output"
        ) && !call_id.is_empty()
            && !present_call_ids.contains(call_id.as_str())
        {
            if let Some(cached) = cache_by_id.get(&call_id) {
                repaired.push(cached.clone());
                present_call_ids.insert(call_id.clone());
                changed = true;
            }
        }
        repaired.push(item);
    }
    if !changed {
        return false;
    }
    *input = repaired;
    true
}

fn call_ids(input: &[Value], types: &[&str]) -> HashSet<String> {
    input
        .iter()
        .filter(|item| {
            item.get("type")
                .and_then(Value::as_str)
                .is_some_and(|item_type| types.contains(&item_type))
        })
        .filter_map(|item| item.get("call_id").and_then(Value::as_str))
        .filter(|call_id| !call_id.is_empty())
        .map(str::to_owned)
        .collect()
}

fn cached_call_map(cached_calls: &[Value]) -> HashMap<String, Value> {
    let mut map = HashMap::new();
    for item in cached_calls {
        let Some(call) = input_call_item(item) else {
            continue;
        };
        let Some(call_id) = call.get("call_id").and_then(Value::as_str) else {
            continue;
        };
        map.insert(call_id.to_owned(), call);
    }
    map
}

fn input_call_item(item: &Value) -> Option<Value> {
    let item_type = item.get("type").and_then(Value::as_str)?;
    if item_type != "function_call" && item_type != "custom_tool_call" {
        return None;
    }
    let call_id = item.get("call_id").and_then(Value::as_str)?;
    if call_id.is_empty() {
        return None;
    }
    let mut out = Map::new();
    out.insert("type".to_owned(), Value::String(item_type.to_owned()));
    out.insert("call_id".to_owned(), Value::String(call_id.to_owned()));
    if let Some(name) = item.get("name") {
        out.insert("name".to_owned(), name.clone());
    }
    if let Some(arguments) = item.get("arguments") {
        out.insert("arguments".to_owned(), arguments.clone());
    }
    if item_type == "custom_tool_call"
        && let Some(input) = item.get("input")
    {
        out.insert("input".to_owned(), input.clone());
    }
    Some(Value::Object(out))
}

pub(super) fn upsert_tool_calls(calls: &mut Vec<Value>, incoming: Vec<Value>) {
    for item in incoming {
        let Some(call) = input_call_item(&item) else {
            continue;
        };
        let Some(call_id) = call.get("call_id").and_then(Value::as_str) else {
            continue;
        };
        if let Some(existing) = calls
            .iter_mut()
            .find(|candidate| candidate.get("call_id").and_then(Value::as_str) == Some(call_id))
        {
            *existing = call;
        } else {
            calls.push(call);
        }
    }
}
