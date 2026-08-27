use std::{collections::HashMap, time::Duration};

use tokio::time::Instant;

use crate::codex_thread_title::{CodexThreadInfo, ThreadInfoReadError, ThreadInfoRow};

pub(super) const SESSION_NAME_BATCH_MIN_INTERVAL: Duration = Duration::from_secs(60);
pub(super) const SESSION_NAME_MAX_BATCH: usize = 128;
const SESSION_NAME_RETRY_DELAYS: [Duration; 3] = [
    Duration::from_secs(60),
    Duration::from_secs(5 * 60),
    Duration::from_secs(10 * 60),
];
const SESSION_NAME_MAX_ATTEMPTS: u8 = 4;

#[allow(clippy::struct_excessive_bools)]
#[derive(Debug)]
pub(super) struct SessionNameEntry {
    pub(super) info: Option<CodexThreadInfo>,
    pub(super) has_success: bool,
    pub(super) attempts: u8,
    pub(super) exhausted: bool,
    pub(super) next_attempt: Instant,
    pub(super) connection_visible: bool,
    pub(super) connection_was_visible: bool,
    pub(super) request_visible: bool,
    pub(super) last_error: Option<ThreadInfoReadError>,
}

impl SessionNameEntry {
    pub(super) const fn new(now: Instant) -> Self {
        Self {
            info: None,
            has_success: false,
            attempts: 0,
            exhausted: false,
            next_attempt: now,
            connection_visible: false,
            connection_was_visible: false,
            request_visible: false,
            last_error: None,
        }
    }

    pub(super) const fn eligible(&self) -> bool {
        self.connection_visible
            || (!self.connection_was_visible && !self.has_success && self.request_visible)
    }
}

#[derive(Debug, Default)]
pub(super) struct SessionNameState {
    pub(super) entries: HashMap<String, SessionNameEntry>,
    pub(super) last_batch_at: Option<Instant>,
}

impl SessionNameState {
    pub(super) fn select_due(&mut self, now: Instant) -> Option<Vec<(String, u8)>> {
        let has_new_due = self.has_new_due(now);
        if self.last_batch_at.is_some_and(|last| {
            now.duration_since(last) < SESSION_NAME_BATCH_MIN_INTERVAL && !has_new_due
        }) {
            return None;
        }
        let mut due_ids = self
            .entries
            .iter()
            .filter(|(_, entry)| entry.eligible() && !entry.exhausted && entry.next_attempt <= now)
            .map(|(thread_id, _)| thread_id.clone())
            .collect::<Vec<_>>();
        due_ids.sort();
        due_ids.truncate(SESSION_NAME_MAX_BATCH);
        let due = due_ids
            .into_iter()
            .filter_map(|thread_id| {
                self.entries.get_mut(&thread_id).map(|entry| {
                    entry.attempts = entry.attempts.saturating_add(1);
                    (thread_id, entry.attempts)
                })
            })
            .collect::<Vec<_>>();
        if due.is_empty() {
            return None;
        }
        self.last_batch_at = Some(now);
        Some(due)
    }

    pub(super) fn apply_result(
        &mut self,
        due: &[(String, u8)],
        result: Result<Vec<ThreadInfoRow>, ThreadInfoReadError>,
        now: Instant,
    ) {
        match result {
            Ok(rows) => {
                let rows = rows
                    .into_iter()
                    .map(|row| (row.thread_id, row.info))
                    .collect::<HashMap<_, _>>();
                for (thread_id, attempt) in due {
                    let Some(entry) = self.entries.get_mut(thread_id) else {
                        continue;
                    };
                    if entry.has_success {
                        entry.info = rows.get(thread_id).cloned();
                        entry.last_error = None;
                        schedule_refresh(entry, now);
                        continue;
                    }
                    let Some(info) = rows.get(thread_id).cloned() else {
                        handle_failure(entry, now, *attempt);
                        continue;
                    };
                    if has_name(&info) {
                        entry.info = Some(info);
                        entry.has_success = true;
                        entry.attempts = 0;
                        entry.exhausted = false;
                        entry.next_attempt = now + super::SESSION_NAME_REFRESH_INTERVAL;
                        entry.last_error = None;
                    } else {
                        entry.info = Some(info);
                        entry.last_error = None;
                        handle_failure(entry, now, *attempt);
                    }
                }
            }
            Err(error) => {
                for (thread_id, attempt) in due {
                    if let Some(entry) = self.entries.get_mut(thread_id) {
                        if entry.has_success {
                            schedule_refresh(entry, now);
                        } else {
                            handle_error(entry, now, *attempt, error);
                        }
                    }
                }
            }
        }
    }

    pub(super) fn has_new_due(&self, now: Instant) -> bool {
        self.entries.values().any(|entry| {
            entry.eligible()
                && !entry.exhausted
                && !entry.has_success
                && entry.attempts == 0
                && entry.next_attempt <= now
        })
    }

    pub(super) fn prune(&mut self) -> bool {
        let before = self.entries.len();
        self.entries
            .retain(|_, entry| entry.connection_visible || entry.request_visible);
        self.entries.len() != before
    }
}

fn has_name(info: &CodexThreadInfo) -> bool {
    info.name
        .as_deref()
        .is_some_and(|name| !name.trim().is_empty())
}

pub(super) fn handle_failure(entry: &mut SessionNameEntry, now: Instant, attempt: u8) {
    if attempt >= SESSION_NAME_MAX_ATTEMPTS {
        entry.exhausted = true;
        return;
    }
    let Some(delay) = SESSION_NAME_RETRY_DELAYS
        .get(usize::from(attempt.saturating_sub(1)))
        .copied()
    else {
        return;
    };
    entry.next_attempt = now + delay;
}

fn handle_error(
    entry: &mut SessionNameEntry,
    now: Instant,
    attempt: u8,
    error: ThreadInfoReadError,
) {
    entry.last_error = Some(error);
    handle_failure(entry, now, attempt);
}

fn schedule_refresh(entry: &mut SessionNameEntry, now: Instant) {
    entry.attempts = 0;
    entry.exhausted = false;
    entry.next_attempt = now + super::SESSION_NAME_REFRESH_INTERVAL;
}
