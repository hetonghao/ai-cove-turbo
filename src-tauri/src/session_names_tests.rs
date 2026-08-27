use super::{
    SESSION_NAME_BATCH_MIN_INTERVAL, SESSION_NAME_MAX_BATCH, SessionNameCache, SessionNameEntry,
};
use crate::proxy::ConnectionSnapshot;
use crate::session_names::state::handle_failure;

use std::{path::PathBuf, time::Duration};

use tokio::time::{self, Instant};

#[test]
fn failure_backoff_uses_one_five_and_ten_minutes_then_stops() {
    let now = Instant::now();
    let mut entry = SessionNameEntry::new(now);
    handle_failure(&mut entry, now, 1);
    assert_eq!(
        entry.next_attempt.duration_since(now),
        Duration::from_secs(60)
    );
    handle_failure(&mut entry, now, 2);
    assert_eq!(
        entry.next_attempt.duration_since(now),
        Duration::from_secs(5 * 60)
    );
    handle_failure(&mut entry, now, 3);
    assert_eq!(
        entry.next_attempt.duration_since(now),
        Duration::from_secs(10 * 60)
    );
    handle_failure(&mut entry, now, 4);
    assert!(entry.exhausted);
}

#[tokio::test(start_paused = true)]
async fn removing_all_connections_enters_cleanup_even_when_request_is_idle() {
    let cache = SessionNameCache::new(PathBuf::from("/missing/state_5.sqlite"));
    let mut state = cache.state.lock().await;
    let now = Instant::now();
    let mut entry = SessionNameEntry::new(now);
    entry.connection_visible = true;
    entry.request_visible = false;
    state
        .entries
        .insert("019fc8b1-a38c-7e70-9169-d6d76a7fcedc".to_owned(), entry);
    drop(state);

    cache
        .observe_connections(&ConnectionSnapshot::default())
        .await;
    let snapshot = cache.snapshot().await;
    assert!(snapshot.is_empty());
    assert_eq!(SESSION_NAME_BATCH_MIN_INTERVAL, Duration::from_secs(60));
    time::advance(Duration::from_secs(1)).await;
}

#[tokio::test(start_paused = true)]
async fn new_request_bypasses_recent_batch_guard_for_first_lookup() {
    let cache = SessionNameCache::new(PathBuf::from("/missing/state_5.sqlite"));
    let thread_id = "019fc8b1-a38c-7e70-9169-d6d76a7fcedc".to_owned();
    let now = Instant::now();
    let mut state = cache.state.lock().await;
    state.last_batch_at = Some(now);
    let mut entry = SessionNameEntry::new(now);
    entry.request_visible = true;
    state.entries.insert(thread_id.clone(), entry);
    drop(state);

    cache.refresh_due().await;

    let state = cache.state.lock().await;
    assert_eq!(state.entries[&thread_id].attempts, 1);
}

#[tokio::test(start_paused = true)]
async fn connection_cleanup_stops_refresh_but_keeps_recent_request_name() {
    let cache = SessionNameCache::new(PathBuf::from("/missing/state_5.sqlite"));
    let thread_id = "019fc8b1-a38c-7e70-9169-d6d76a7fcedc".to_owned();
    let now = Instant::now();
    let mut state = cache.state.lock().await;
    let mut entry = SessionNameEntry::new(now);
    entry.connection_visible = true;
    entry.connection_was_visible = true;
    entry.request_visible = true;
    entry.has_success = true;
    entry.info = Some(crate::codex_thread_title::CodexThreadInfo {
        name: Some("历史会话".to_owned()),
        parent_name: None,
        is_subagent: false,
    });
    state.entries.insert(thread_id.clone(), entry);
    drop(state);

    cache
        .observe_connections(&ConnectionSnapshot::default())
        .await;

    let snapshot = cache.snapshot().await;
    assert_eq!(
        snapshot[&thread_id]
            .as_ref()
            .and_then(|info| info.name.as_deref()),
        Some("历史会话")
    );
    let state = cache.state.lock().await;
    assert!(!state.entries[&thread_id].eligible());
}

#[tokio::test(start_paused = true)]
async fn only_queried_batch_entries_consume_an_attempt() {
    let cache = SessionNameCache::new(PathBuf::from("/missing/state_5.sqlite"));
    let now = Instant::now();
    let mut state = cache.state.lock().await;
    for index in 0..=SESSION_NAME_MAX_BATCH {
        let thread_id = format!("019fc8b1-a38c-7e70-9169-d6d76a7f{index:04x}");
        let mut entry = SessionNameEntry::new(now);
        entry.request_visible = true;
        state.entries.insert(thread_id, entry);
    }
    drop(state);

    cache.refresh_due().await;

    let state = cache.state.lock().await;
    assert_eq!(
        state
            .entries
            .values()
            .filter(|entry| entry.attempts == 1)
            .count(),
        SESSION_NAME_MAX_BATCH
    );
    assert_eq!(
        state
            .entries
            .values()
            .filter(|entry| entry.attempts == 0)
            .count(),
        1
    );
}
