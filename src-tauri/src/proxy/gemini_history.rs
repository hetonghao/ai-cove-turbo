use std::collections::{HashMap, HashSet};

use serde_json::Value;

pub(super) fn normalize_gemini_function_history(payload: &[u8]) -> Option<Vec<u8>> {
    let mut request: Value = serde_json::from_slice(payload).ok()?;
    let is_gemini = request
        .get("model")
        .and_then(Value::as_str)
        .is_some_and(|model| model.starts_with("gemini-"));
    if !is_gemini {
        return None;
    }
    let input = request.get_mut("input").and_then(Value::as_array_mut)?;
    let mut call_ids = HashSet::new();
    let mut output_ids = HashSet::new();
    for item in input.iter() {
        let ids = match item.get("type").and_then(Value::as_str) {
            Some("function_call") => Some(&mut call_ids),
            Some("function_call_output") => Some(&mut output_ids),
            _ => None,
        };
        let Some(ids) = ids else {
            continue;
        };
        let call_id = item.get("call_id").and_then(Value::as_str)?;
        if call_id.is_empty() || !ids.insert(call_id) {
            return None;
        }
    }
    if call_ids != output_ids {
        return None;
    }
    if call_ids.is_empty() {
        return None;
    }
    if !validate_segments(input) {
        return None;
    }
    if !normalize_input(input) {
        return None;
    }
    serde_json::to_vec(&request).ok()
}

fn validate_segments(input: &[Value]) -> bool {
    let mut call_ids = Vec::new();
    let mut output_ids = Vec::new();
    let mut output_started = false;

    for item in input {
        match item.get("type").and_then(Value::as_str) {
            Some("function_call") => {
                if output_started {
                    return false;
                }
                let Some(call_id) = item.get("call_id").and_then(Value::as_str) else {
                    return false;
                };
                call_ids.push(call_id);
            }
            Some("message") if !call_ids.is_empty() => {
                if item.get("role").and_then(Value::as_str) != Some("assistant") {
                    return false;
                }
            }
            Some("function_call_output") => {
                if call_ids.is_empty() {
                    return false;
                }
                let Some(call_id) = item.get("call_id").and_then(Value::as_str) else {
                    return false;
                };
                if !call_ids.iter().any(|candidate| candidate == &call_id)
                    || output_ids.iter().any(|candidate| candidate == &call_id)
                {
                    return false;
                }
                output_started = true;
                output_ids.push(call_id);
                if output_ids.len() == call_ids.len() {
                    call_ids.clear();
                    output_ids.clear();
                    output_started = false;
                }
            }
            _ if !call_ids.is_empty() => return false,
            _ => {}
        }
    }
    call_ids.is_empty()
}

fn normalize_input(input: &mut Vec<Value>) -> bool {
    let source = input.clone();
    let output_by_call = output_by_call(&source);
    let mut normalized = Vec::with_capacity(source.len());
    let mut consumed_outputs = HashSet::new();
    let mut changed = false;
    let mut index = 0;
    while let Some(item) = source.get(index) {
        if is_type(item, "function_call") {
            let Some((next_index, group_changed)) = normalize_call_group(
                &source,
                index,
                &output_by_call,
                &mut consumed_outputs,
                &mut normalized,
            ) else {
                return false;
            };
            changed |= group_changed;
            index = next_index;
            continue;
        }
        if is_type(item, "function_call_output")
            && item
                .get("call_id")
                .and_then(Value::as_str)
                .is_some_and(|call_id| consumed_outputs.contains(call_id))
        {
            index += 1;
            continue;
        }
        normalized.push(item.clone());
        index += 1;
    }
    if normalized.len() != source.len() || !changed {
        return false;
    }
    *input = normalized;
    true
}

fn output_by_call(source: &[Value]) -> HashMap<String, Value> {
    source
        .iter()
        .filter(|item| is_type(item, "function_call_output"))
        .filter_map(|item| {
            item.get("call_id")
                .and_then(Value::as_str)
                .map(|call_id| (call_id.to_owned(), item.clone()))
        })
        .collect()
}

fn normalize_call_group(
    source: &[Value],
    index: usize,
    output_by_call: &HashMap<String, Value>,
    consumed_outputs: &mut HashSet<String>,
    normalized: &mut Vec<Value>,
) -> Option<(usize, bool)> {
    let group_end = function_call_group_end(source, index);
    let group = source.get(index..group_end)?;
    let call_ids = group
        .iter()
        .map(|call| {
            call.get("call_id")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect::<Option<Vec<_>>>()?;
    if call_ids.len() == 1 {
        return normalize_single_call(
            source,
            index,
            &call_ids,
            output_by_call,
            consumed_outputs,
            normalized,
        );
    }

    let (cursor, encountered_outputs, deferred_messages) =
        collect_call_group_tail(source, group_end, &call_ids)?;
    normalized.extend(group.iter().cloned());
    for call_id in &call_ids {
        normalized.push(output_by_call.get(call_id)?.clone());
        consumed_outputs.insert(call_id.clone());
    }
    normalized.extend(deferred_messages.iter().cloned());
    let changed = encountered_outputs != call_ids || !deferred_messages.is_empty();
    Some((cursor, changed))
}

fn function_call_group_end(source: &[Value], index: usize) -> usize {
    let mut end = index;
    while source
        .get(end)
        .is_some_and(|item| is_type(item, "function_call"))
    {
        end += 1;
    }
    end
}

fn normalize_single_call(
    source: &[Value],
    index: usize,
    call_ids: &[String],
    output_by_call: &HashMap<String, Value>,
    consumed_outputs: &mut HashSet<String>,
    normalized: &mut Vec<Value>,
) -> Option<(usize, bool)> {
    let call_id = call_ids.first()?;
    let output = output_by_call.get(call_id)?;
    normalized.push(source.get(index)?.clone());
    normalized.push(output.clone());
    consumed_outputs.insert(call_id.clone());
    let output_is_adjacent = source.get(index + 1).is_some_and(|next| {
        is_type(next, "function_call_output")
            && next.get("call_id").and_then(Value::as_str) == Some(call_id)
    });
    Some((index + 1, !output_is_adjacent))
}

fn collect_call_group_tail(
    source: &[Value],
    mut cursor: usize,
    call_ids: &[String],
) -> Option<(usize, Vec<String>, Vec<Value>)> {
    let mut encountered_outputs = Vec::with_capacity(call_ids.len());
    let mut encountered_ids = HashSet::new();
    let mut deferred_messages = Vec::new();
    while encountered_outputs.len() < call_ids.len() {
        let next = source.get(cursor)?;
        match next.get("type").and_then(Value::as_str) {
            Some("message") if next.get("role").and_then(Value::as_str) == Some("assistant") => {
                deferred_messages.push(next.clone());
            }
            Some("function_call_output") => {
                let call_id = next.get("call_id").and_then(Value::as_str)?;
                if !call_ids.iter().any(|candidate| candidate == call_id)
                    || !encountered_ids.insert(call_id)
                {
                    return None;
                }
                encountered_outputs.push(call_id.to_owned());
            }
            _ => return None,
        }
        cursor += 1;
    }
    Some((cursor, encountered_outputs, deferred_messages))
}

fn is_type(item: &Value, expected: &str) -> bool {
    item.get("type").and_then(Value::as_str) == Some(expected)
}
