use std::time::Duration;

use serde::Serialize;
use tokio::time::Instant;

use super::{HybridPool, PoolState, scope::HybridScope, scope::ScopeFingerprint};

#[path = "observability_snapshot.rs"]
mod snapshot;

const RECENT_CLOSED_LIMIT: usize = 8;
const RECENT_CLOSED_WINDOW: Duration = Duration::from_secs(5 * 60);

fn recent_closed_visible(age: Duration) -> bool {
    age < RECENT_CLOSED_WINDOW
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ConnectionActivity {
    Idle,
    Up,
    Down,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum SessionReclaimPolicy {
    ThreadEnd,
}

#[derive(Clone, Debug)]
pub(crate) enum ConnectionObservation {
    Bound { thread_id: String },
    Active(ConnectionActivity),
    Idle,
}

#[derive(Clone, Debug)]
pub(in crate::proxy) enum LeaseRetirement {
    Recovering { reason: String },
    Replacing,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConnectionSnapshot {
    pub(crate) current_connections: usize,
    pub(crate) prewarm: usize,
    pub(crate) bound_threads: Vec<BoundThreadConnection>,
    pub(crate) transitions: Vec<ConnectionTransition>,
    pub(crate) recent_closed: Vec<ClosedConnection>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BoundThreadConnection {
    pub(crate) id: String,
    pub(crate) thread_id: String,
    pub(crate) activity: ConnectionActivity,
    pub(crate) idle_seconds: u64,
    pub(crate) reclaim_policy: SessionReclaimPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) upstream_trace: Option<String>,
    pub(crate) upstream_generation: u64,
    pub(crate) upstream_ordinal: u64,
    pub(crate) connection_age_seconds: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) last_probe_age_seconds: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConnectionTransition {
    pub(crate) id: String,
    pub(crate) thread_id: Option<String>,
    pub(crate) connection_id: Option<String>,
    pub(crate) label: String,
    pub(crate) stage: String,
    pub(crate) detail: String,
    pub(crate) elapsed_seconds: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ClosedConnection {
    pub(crate) id: String,
    pub(crate) thread_id: Option<String>,
    pub(crate) connection_id: String,
    pub(crate) reason: String,
    pub(crate) ago_seconds: u64,
    pub(crate) normal: bool,
}

#[derive(Debug)]
pub(super) struct ObservedSession {
    scope_fingerprint: ScopeFingerprint,
    thread_id: Option<String>,
    state: ObservedSessionState,
    updated_at: Instant,
}

#[derive(Debug)]
enum ObservedSessionState {
    Idle,
    Active(ConnectionActivity),
    Recovering(String),
}

#[derive(Debug)]
pub(super) struct ClosedRecord {
    id: u64,
    connection_id: u64,
    thread_id: Option<String>,
    reason: String,
    closed_at: Instant,
    normal: bool,
}

impl PoolState {
    pub(super) const fn allocate_connection_id(&mut self) -> u64 {
        self.next_connection_id = self.next_connection_id.saturating_add(1);
        self.next_connection_id
    }

    pub(super) fn register_session(&mut self, scope_fingerprint: ScopeFingerprint) -> u64 {
        self.next_session_id = self.next_session_id.saturating_add(1);
        let session_id = self.next_session_id;
        self.sessions.insert(
            session_id,
            ObservedSession {
                scope_fingerprint,
                thread_id: None,
                state: ObservedSessionState::Idle,
                updated_at: Instant::now(),
            },
        );
        session_id
    }

    pub(super) fn push_closed(
        &mut self,
        connection_id: u64,
        thread_id: Option<String>,
        reason: String,
        normal: bool,
    ) {
        self.next_closed_id = self.next_closed_id.saturating_add(1);
        self.recent_closed.push_front(ClosedRecord {
            id: self.next_closed_id,
            connection_id,
            thread_id,
            reason,
            closed_at: Instant::now(),
            normal,
        });
        self.recent_closed.truncate(RECENT_CLOSED_LIMIT);
    }

    pub(super) fn release_session(&mut self, session_id: u64, connection_id: Option<u64>) {
        let thread_id = self
            .sessions
            .get(&session_id)
            .and_then(|session| session.thread_id.clone());
        if let Some(connection_id) = connection_id {
            self.push_closed(connection_id, thread_id, "Codex 线程结束".to_owned(), true);
        }
        if let Some(session) = self.sessions.get_mut(&session_id) {
            session.state = ObservedSessionState::Idle;
            session.updated_at = Instant::now();
        }
    }

    pub(super) fn retire_session(
        &mut self,
        session_id: u64,
        connection_id: Option<u64>,
        retirement: LeaseRetirement,
    ) {
        let Some(session) = self.sessions.get_mut(&session_id) else {
            return;
        };
        let thread_id = session.thread_id.clone();
        match retirement {
            LeaseRetirement::Recovering { reason } => {
                session.state = ObservedSessionState::Recovering(reason.clone());
                session.updated_at = Instant::now();
                if let Some(connection_id) = connection_id {
                    self.push_closed(connection_id, thread_id, reason, false);
                }
            }
            LeaseRetirement::Replacing => {
                session.state = ObservedSessionState::Idle;
                session.updated_at = Instant::now();
            }
        }
    }

    pub(super) fn remove_session(&mut self, session_id: u64, connection_id: Option<u64>) {
        let thread_id = self
            .sessions
            .remove(&session_id)
            .and_then(|session| session.thread_id);
        if let Some(connection_id) = connection_id {
            self.push_closed(connection_id, thread_id, "Codex 线程结束".to_owned(), true);
        }
    }
}

impl HybridPool {
    pub(in crate::proxy) async fn record_response_create(
        &self,
        scope: &HybridScope,
        session_id: u64,
    ) {
        let mut state = self.inner.state.lock().await;
        let Some(lease) = state
            .scopes
            .get_mut(scope)
            .and_then(|entry| entry.leased.get_mut(&session_id))
        else {
            return;
        };
        lease.ordinal = lease.ordinal.saturating_add(1);
        drop(state);
    }

    pub(crate) async fn observe_session(
        &self,
        session_id: u64,
        observation: ConnectionObservation,
    ) {
        let mut state = self.inner.state.lock().await;
        let Some(session) = state.sessions.get_mut(&session_id) else {
            return;
        };
        match observation {
            ConnectionObservation::Bound { thread_id } => {
                session.thread_id = Some(thread_id);
                session.state = ObservedSessionState::Idle;
            }
            ConnectionObservation::Active(activity) => {
                session.state = ObservedSessionState::Active(activity);
            }
            ConnectionObservation::Idle => {
                session.state = ObservedSessionState::Idle;
            }
        }
        session.updated_at = Instant::now();
        drop(state);
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{RECENT_CLOSED_LIMIT, RECENT_CLOSED_WINDOW, recent_closed_visible};

    #[test]
    fn recent_closed_window_is_exactly_five_minutes() {
        assert_eq!(RECENT_CLOSED_LIMIT, 8);
        assert_eq!(RECENT_CLOSED_WINDOW, Duration::from_secs(5 * 60));
        assert!(recent_closed_visible(Duration::from_secs(5 * 60 - 1)));
        assert!(!recent_closed_visible(Duration::from_secs(5 * 60)));
    }
}
