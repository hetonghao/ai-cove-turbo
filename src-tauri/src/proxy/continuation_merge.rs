use std::collections::{BTreeMap, HashMap, HashSet};

use serde_json::Value;

/// 展开时由 Turbo 自己重建的字段，不参与“从上一轮请求继承”。
const REBUILT_KEYS: [&str; 4] = ["type", "input", "previous_response_id", "stream"];

const TOOL_CALL_TYPES: [&str; 2] = ["function_call", "custom_tool_call"];

/// 一次 HTTP 响应期间累积的续传材料：请求体 + 响应产出的 output items。
#[derive(Default)]
pub(super) struct PendingHttpContinuation {
    request: Option<Value>,
    indexed_items: BTreeMap<i64, Value>,
    unindexed_items: Vec<Value>,
    terminal_output: Vec<Value>,
}

/// 上一轮已经完成的 HTTP 往返，可作为状态续传的展开底稿。
pub(super) struct HttpContinuation {
    request: Value,
    output: Vec<Value>,
}

impl PendingHttpContinuation {
    pub(super) fn new(payload: &[u8]) -> Self {
        Self {
            request: serde_json::from_slice(payload).ok(),
            ..Self::default()
        }
    }

    /// 收集响应事件里的 output items。终态事件的 `response.output` 是权威清单。
    pub(super) fn observe(&mut self, payload: &[u8]) {
        let Ok(event) = serde_json::from_slice::<Value>(payload) else {
            return;
        };
        match event.get("type").and_then(Value::as_str) {
            Some("response.output_item.done") => {
                let Some(item) = event.get("item").filter(|item| item.is_object()) else {
                    return;
                };
                match event.get("output_index").and_then(Value::as_i64) {
                    Some(index) => {
                        self.indexed_items.insert(index, item.clone());
                    }
                    None => self.unindexed_items.push(item.clone()),
                }
            }
            Some("response.completed" | "response.done") => {
                let Some(output) = event.pointer("/response/output").and_then(Value::as_array)
                else {
                    return;
                };
                self.terminal_output.clone_from(output);
            }
            _ => {}
        }
    }

    pub(super) fn finish(self) -> Option<HttpContinuation> {
        let request = self.request?;
        let output = if self.terminal_output.is_empty() {
            self.indexed_items
                .into_values()
                .chain(self.unindexed_items)
                .collect()
        } else {
            self.terminal_output
        };
        Some(HttpContinuation { request, output })
    }
}

impl HttpContinuation {
    /// 把增量续传帧展开成自包含请求；形态不认识时返回 `None`。
    pub(super) fn expand(&self, payload: &[u8]) -> Option<Vec<u8>> {
        expand(&self.request, &self.output, payload)
    }
}

/// 把带 `previous_response_id` 的增量续传帧展开成自包含请求。
///
/// 只支持 HTTP 的上游不会解析 `previous_response_id`：一个只带
/// `custom_tool_call_output` 的增量帧会被判成“找不到配对的 tool call”而整轮 400。
/// 展开结果与客户端自己用全量历史重发等价——以上一轮实际发往上游的请求为底，
/// `input` 换成「上一轮 input + 上一轮响应 output + 本次增量」，删掉
/// `previous_response_id`，因此上游不需要任何状态回放。
pub(super) fn expand(
    previous_request: &Value,
    previous_output: &[Value],
    payload: &[u8],
) -> Option<Vec<u8>> {
    let previous_input = previous_request.get("input")?.as_array()?;
    let continuation: Value = serde_json::from_slice(payload).ok()?;
    let continuation = continuation.as_object()?;
    let delta = continuation.get("input")?.as_array()?;

    let merged = merge_input(previous_input, previous_output, delta);
    // 展开结果必须与上游一样能自证成对：拿不到某个 tool call 时宁可让客户端全量重发。
    if has_unpaired_tool_output(&merged) {
        return None;
    }

    let mut expanded = previous_request.clone();
    let object = expanded.as_object_mut()?;
    object.remove("type");
    object.remove("previous_response_id");
    object.insert("input".to_owned(), Value::Array(merged));
    for (key, value) in continuation {
        if REBUILT_KEYS.contains(&key.as_str()) {
            continue;
        }
        object.insert(key.clone(), value.clone());
    }
    object.insert("stream".to_owned(), Value::Bool(true));
    serde_json::to_vec(&expanded).ok()
}

struct MergeItem {
    value: Value,
    item_type: String,
    id: String,
    call_id: String,
}

impl MergeItem {
    fn new(value: &Value) -> Self {
        let text = |key: &str| {
            value
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        Self {
            value: value.clone(),
            item_type: text("type"),
            id: text("id"),
            call_id: text("call_id"),
        }
    }

    fn is_tool_call(&self) -> bool {
        TOOL_CALL_TYPES.contains(&self.item_type.as_str())
    }
}

pub(super) fn merge_input(
    previous_input: &[Value],
    previous_output: &[Value],
    delta: &[Value],
) -> Vec<Value> {
    let mut items: Vec<MergeItem> = previous_input
        .iter()
        .chain(previous_output)
        .chain(delta)
        .map(MergeItem::new)
        .collect();
    dedupe_tool_calls(&mut items);
    dedupe_by_id(&mut items);
    items.into_iter().map(|item| item.value).collect()
}

/// 合并后的条目里是否存在找不到配对调用的 tool 输出（调用必须排在输出之前）。
fn has_unpaired_tool_output(items: &[Value]) -> bool {
    let mut calls = HashSet::new();
    for item in items {
        let Some(item_type) = item.get("type").and_then(Value::as_str) else {
            continue;
        };
        let Some(call_id) = item
            .get("call_id")
            .and_then(Value::as_str)
            .filter(|call_id| !call_id.is_empty())
        else {
            continue;
        };
        match item_type {
            "function_call" | "custom_tool_call" => {
                calls.insert(call_id);
            }
            "function_call_output" | "custom_tool_call_output" if !calls.contains(call_id) => {
                return true;
            }
            _ => {}
        }
    }
    false
}

/// 同一个 `call_id` 的 tool call 只保留第一次出现，调用始终排在配对输出之前。
fn dedupe_tool_calls(items: &mut Vec<MergeItem>) {
    let mut seen = HashSet::new();
    items.retain(|item| {
        if !item.is_tool_call() || item.call_id.is_empty() {
            return true;
        }
        seen.insert(item.call_id.clone())
    });
}

/// 同一个 `id` 只保留最后一次出现，本次增量总是覆盖历史副本。
fn dedupe_by_id(items: &mut Vec<MergeItem>) {
    let mut last_index: HashMap<String, usize> = HashMap::new();
    for (index, item) in items.iter().enumerate() {
        if !item.id.is_empty() {
            last_index.insert(item.id.clone(), index);
        }
    }
    let mut index = 0;
    items.retain(|item| {
        let keep = item.id.is_empty() || last_index.get(&item.id) == Some(&index);
        index += 1;
        keep
    });
}
