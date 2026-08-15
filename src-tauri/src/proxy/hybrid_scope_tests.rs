use axum::http::{HeaderMap, HeaderValue, header};

use super::*;

fn metadata_headers(thread: &'static str, session: &'static str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::AUTHORIZATION,
        HeaderValue::from_static("Bearer account"),
    );
    headers.insert("session-id", HeaderValue::from_static(session));
    headers.insert("thread-id", HeaderValue::from_static(thread));
    headers.insert("x-client-request-id", HeaderValue::from_static(thread));
    headers.insert("x-codex-window-id", HeaderValue::from_static(thread));
    headers.insert(
        "x-codex-installation-id",
        HeaderValue::from_static("installation"),
    );
    headers.insert(
        "x-codex-turn-metadata",
        HeaderValue::from_static(r#"{"thread_id":"thread"}"#),
    );
    headers.insert(
        "x-ai-cove-ws-trace",
        HeaderValue::from_static("client-controlled-trace"),
    );
    headers
}

#[test]
fn dynamic_codex_metadata_shares_blank_pool_scope() {
    let target = Url::parse("https://api.ai-cove.com/v1/responses").expect("valid URL");
    let mut parent = metadata_headers("parent", "root");
    let mut child = metadata_headers("child", "root");
    parent.insert("x-codex-turn-state", HeaderValue::from_static("sticky"));
    child.insert("x-codex-turn-state", HeaderValue::from_static("sticky"));
    child.insert(
        "x-codex-parent-thread-id",
        HeaderValue::from_static("parent"),
    );
    child.insert(
        "x-openai-subagent",
        HeaderValue::from_static("collab_spawn"),
    );

    assert_eq!(
        HybridScope::new(&target, &parent),
        HybridScope::new(&target, &child)
    );
}

#[test]
fn authorization_and_turn_state_still_isolate_pool_scope() {
    let target = Url::parse("https://api.ai-cove.com/v1/responses").expect("valid URL");
    let mut first = metadata_headers("first", "root");
    let mut other_authorization = metadata_headers("second", "root");
    let mut other_turn_state = metadata_headers("third", "root");
    first.insert("x-codex-turn-state", HeaderValue::from_static("state-a"));
    other_authorization.insert(
        header::AUTHORIZATION,
        HeaderValue::from_static("Bearer other"),
    );
    other_authorization.insert("x-codex-turn-state", HeaderValue::from_static("state-a"));
    other_turn_state.insert("x-codex-turn-state", HeaderValue::from_static("state-b"));

    let first = HybridScope::new(&target, &first);
    assert_ne!(first, HybridScope::new(&target, &other_authorization));
    assert_ne!(first, HybridScope::new(&target, &other_turn_state));
}

#[test]
fn blank_connection_headers_remove_dynamic_session_metadata() {
    let mut headers = metadata_headers("child", "root");
    headers.insert("x-codex-turn-state", HeaderValue::from_static("sticky"));
    headers.insert(
        "x-codex-parent-thread-id",
        HeaderValue::from_static("parent"),
    );
    headers.insert(
        "x-openai-subagent",
        HeaderValue::from_static("collab_spawn"),
    );

    let blank = blank_connection_headers(&headers);

    assert_eq!(
        blank.get(header::AUTHORIZATION),
        Some(&HeaderValue::from_static("Bearer account"))
    );
    assert_eq!(
        blank.get("x-codex-turn-state"),
        Some(&HeaderValue::from_static("sticky"))
    );
    for dynamic in [
        "session-id",
        "thread-id",
        "x-client-request-id",
        "x-codex-installation-id",
        "x-codex-window-id",
        "x-codex-turn-metadata",
        "x-codex-parent-thread-id",
        "x-openai-subagent",
        "x-ai-cove-ws-trace",
    ] {
        assert!(blank.get(dynamic).is_none());
    }
}
