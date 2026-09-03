use std::io;

use serde_json::Value;

use super::gemini_history::normalize_gemini_function_history;

fn item_types(payload: &[u8]) -> io::Result<Vec<String>> {
    let value: Value = serde_json::from_slice(payload).map_err(io::Error::other)?;
    value
        .get("input")
        .and_then(Value::as_array)
        .ok_or_else(|| io::Error::other("normalized input is missing"))
        .map(|input| {
            input
                .iter()
                .filter_map(|item| item.get("type").and_then(Value::as_str).map(str::to_owned))
                .collect()
        })
}

fn item_identity(payload: &[u8]) -> io::Result<Vec<(Option<String>, Option<String>)>> {
    let value: Value = serde_json::from_slice(payload).map_err(io::Error::other)?;
    value
        .get("input")
        .and_then(Value::as_array)
        .ok_or_else(|| io::Error::other("normalized input is missing"))
        .map(|input| {
            input
                .iter()
                .map(|item| {
                    (
                        item.get("type").and_then(Value::as_str).map(str::to_owned),
                        item.get("call_id")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                    )
                })
                .collect()
        })
}

#[test]
fn gemini_http_history_moves_tool_output_before_assistant_content() -> io::Result<()> {
    // Given: a Gemini Responses history places assistant text before its pending tool output.
    let payload = br#"{"model":"gemini-3.8-flash","input":[{"type":"function_call","call_id":"call-1"},{"type":"message","role":"assistant","content":"working"},{"type":"function_call_output","call_id":"call-1","output":"ok"}]}"#;

    // When: Turbo prepares the history for the Gemini upstream.
    let normalized = normalize_gemini_function_history(payload)
        .ok_or_else(|| io::Error::other("Gemini history was not normalized"))?;

    // Then: every pending function call is answered before assistant content resumes.
    assert_eq!(
        item_types(&normalized)?,
        ["function_call", "function_call_output", "message"]
    );
    Ok(())
}

#[test]
fn ambiguous_gemini_call_ids_leave_history_unchanged() {
    // Given: one Gemini call has duplicate candidate outputs.
    let payload = br#"{"model":"gemini-3.8-flash","input":[{"type":"function_call","call_id":"call-1"},{"type":"message","role":"assistant","content":"working"},{"type":"function_call_output","call_id":"call-1","output":"first"},{"type":"function_call_output","call_id":"call-1","output":"second"}]}"#;

    // When: Turbo checks whether the history has one unambiguous repair.
    let normalized = normalize_gemini_function_history(payload);

    // Then: Turbo does not guess between duplicate outputs.
    assert!(normalized.is_none());
}

#[test]
fn websocket_initial_and_continuation_normalize_the_same_history() -> io::Result<()> {
    // Given: initial and continuation response.create frames carry malformed Gemini history.
    let payloads = [
        br#"{"type":"response.create","model":"gemini-3.8-flash","input":[{"type":"function_call","call_id":"call-1"},{"type":"message","role":"assistant","content":"working"},{"type":"function_call_output","call_id":"call-1","output":"ok"}]}"#.as_slice(),
        br#"{"type":"response.create","model":"gemini-3.8-flash","previous_response_id":"response-1","input":[{"type":"function_call","call_id":"call-1"},{"type":"message","role":"assistant","content":"working"},{"type":"function_call_output","call_id":"call-1","output":"ok"}]}"#.as_slice(),
    ];

    // When: Turbo normalizes either WebSocket request shape.
    for payload in payloads {
        let normalized = normalize_gemini_function_history(payload)
            .ok_or_else(|| io::Error::other("Gemini history was not normalized"))?;

        // Then: each frame resolves the call before assistant content.
        assert_eq!(
            item_types(&normalized)?,
            ["function_call", "function_call_output", "message"]
        );
    }
    Ok(())
}

#[test]
fn non_target_or_unsafe_history_stays_byte_for_byte_unchanged() {
    // Given: GPT, Codex, Grok, incomplete Gemini, and user-boundary Gemini histories.
    let payloads = [
        br#"{"model":"gpt-5.6-sol","input":[{"type":"function_call","call_id":"call-1"},{"type":"message","role":"assistant"},{"type":"function_call_output","call_id":"call-1"}]}"#.as_slice(),
        br#"{"model":"codex-mini-latest","input":[{"type":"function_call","call_id":"call-1"},{"type":"message","role":"assistant"},{"type":"function_call_output","call_id":"call-1"}]}"#.as_slice(),
        br#"{"model":"grok-4.6","input":[{"type":"function_call","call_id":"call-1"},{"type":"message","role":"assistant"},{"type":"function_call_output","call_id":"call-1"}]}"#.as_slice(),
        br#"{"model":"gemini-3.8-flash","input":[{"type":"function_call","call_id":"call-1"},{"type":"message","role":"assistant"}]}"#.as_slice(),
        br#"{"model":"gemini-3.8-flash","input":[{"type":"function_call","call_id":"call-1"},{"type":"message","role":"assistant"},{"type":"message","role":"user"},{"type":"function_call_output","call_id":"call-1"}]}"#.as_slice(),
    ];

    // When: Turbo checks each request for the narrow Gemini repair.
    let normalized = payloads.map(normalize_gemini_function_history);

    // Then: none receives a rewritten payload.
    assert!(normalized.into_iter().all(|payload| payload.is_none()));
}

#[test]
fn separated_calls_keep_the_intervening_message_in_place() -> io::Result<()> {
    // Given: two singleton call groups are separated by assistant content.
    let payload = br#"{"model":"gemini-3.8-flash","input":[{"type":"function_call","call_id":"call-1"},{"type":"message","role":"assistant"},{"type":"function_call","call_id":"call-2"},{"type":"function_call_output","call_id":"call-1"},{"type":"function_call_output","call_id":"call-2"}]}"#;

    // When: Turbo normalizes the complete history.
    let normalized = normalize_gemini_function_history(payload)
        .ok_or_else(|| io::Error::other("Gemini history was not normalized"))?;

    // Then: only outputs move; calls and message keep their original relative order.
    assert_eq!(
        item_identity(&normalized)?,
        [
            (Some("function_call".to_owned()), Some("call-1".to_owned())),
            (
                Some("function_call_output".to_owned()),
                Some("call-1".to_owned())
            ),
            (Some("message".to_owned()), None),
            (Some("function_call".to_owned()), Some("call-2".to_owned())),
            (
                Some("function_call_output".to_owned()),
                Some("call-2".to_owned())
            ),
        ]
    );
    Ok(())
}

#[test]
fn adjacent_calls_emit_outputs_in_call_order_before_messages() -> io::Result<()> {
    // Given: adjacent calls have a message before outputs in reverse output order.
    let payload = br#"{"model":"gemini-3.8-flash","input":[{"type":"function_call","call_id":"call-1"},{"type":"function_call","call_id":"call-2"},{"type":"message","role":"assistant"},{"type":"function_call_output","call_id":"call-2"},{"type":"function_call_output","call_id":"call-1"}]}"#;

    // When: Turbo normalizes the adjacent-call group.
    let normalized = normalize_gemini_function_history(payload)
        .ok_or_else(|| io::Error::other("Gemini history was not normalized"))?;

    // Then: calls stay adjacent, outputs use call order, and message follows them.
    assert_eq!(
        item_identity(&normalized)?,
        [
            (Some("function_call".to_owned()), Some("call-1".to_owned())),
            (Some("function_call".to_owned()), Some("call-2".to_owned())),
            (
                Some("function_call_output".to_owned()),
                Some("call-1".to_owned())
            ),
            (
                Some("function_call_output".to_owned()),
                Some("call-2".to_owned())
            ),
            (Some("message".to_owned()), None),
        ]
    );
    Ok(())
}
