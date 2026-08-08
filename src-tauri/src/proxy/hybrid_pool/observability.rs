use std::time::Instant;

use serde::Serialize;

use super::{HybridPool, PoolState};

const RECENT_CLOSED_LIMIT: usize = 12;

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
    Bound {
        thread_id: String,
        has_connection: bool,
    },
    Active(ConnectionActivity),
    Idle,
    Recovering {
        reason: String,
    },
    Closed {
        reason: String,
        normal: bool,
    },
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConnectionSnapshot {
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
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConnectionTransition {
    pub(crate) id: String,
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
    pub(crate) reason: String,
    pub(crate) ago_seconds: u64,
    pub(crate) normal: bool,
}

#[derive(Debug)]
pub(super) struct ObservedSession {
    thread_id: Option<String>,
    has_connection: bool,
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
    thread_id: Option<String>,
    reason: String,
    closed_at: Instant,
    normal: bool,
}

impl PoolState {
    fn register_observed_session(&mut self) -> u64 {
        self.next_session_id = self.next_session_id.saturating_add(1);
        let session_id = self.next_session_id;
        self.sessions.insert(
            session_id,
            ObservedSession {
                thread_id: None,
                has_connection: false,
                state: ObservedSessionState::Idle,
                updated_at: Instant::now(),
            },
        );
        session_id
    }

    fn push_closed(&mut self, thread_id: Option<String>, reason: String, normal: bool) {
        self.next_closed_id = self.next_closed_id.saturating_add(1);
        self.recent_closed.push_front(ClosedRecord {
            id: self.next_closed_id,
            thread_id,
            reason,
            closed_at: Instant::now(),
            normal,
        });
        self.recent_closed.truncate(RECENT_CLOSED_LIMIT);
    }
}

impl HybridPool {
    pub(crate) async fn register_observed_session(&self) -> u64 {
        self.inner.state.lock().await.register_observed_session()
    }

    pub(crate) async fn observe_session(
        &self,
        session_id: u64,
        observation: ConnectionObservation,
    ) {
        let mut state = self.inner.state.lock().await;
        if let ConnectionObservation::Closed { reason, normal } = observation {
            let Some(session) = state.sessions.remove(&session_id) else {
                return;
            };
            if session.thread_id.is_some() && session.has_connection {
                state.push_closed(session.thread_id, reason, normal);
            }
            return;
        }

        let mut closed = None;
        let Some(session) = state.sessions.get_mut(&session_id) else {
            return;
        };
        match observation {
            ConnectionObservation::Bound {
                thread_id,
                has_connection,
            } => {
                session.thread_id = Some(thread_id);
                session.has_connection = has_connection;
                session.state = ObservedSessionState::Idle;
            }
            ConnectionObservation::Active(activity) => {
                session.has_connection = true;
                session.state = ObservedSessionState::Active(activity);
            }
            ConnectionObservation::Idle => {
                session.has_connection = true;
                session.state = ObservedSessionState::Idle;
            }
            ConnectionObservation::Recovering { reason } => {
                if session.has_connection {
                    closed = Some((session.thread_id.clone(), reason.clone()));
                }
                session.has_connection = false;
                session.state = ObservedSessionState::Recovering(reason);
            }
            ConnectionObservation::Closed { .. } => unreachable!(),
        }
        session.updated_at = Instant::now();
        if let Some((thread_id, reason)) = closed {
            state.push_closed(thread_id, reason, false);
        }
    }

    pub(crate) async fn connection_snapshot(&self) -> ConnectionSnapshot {
        let state = self.inner.state.lock().await;
        let now = Instant::now();
        let mut snapshot = ConnectionSnapshot {
            prewarm: state.scopes.values().map(|scope| scope.idle.len()).sum(),
            ..ConnectionSnapshot::default()
        };

        for (session_id, session) in &state.sessions {
            let Some(thread_id) = session.thread_id.as_ref() else {
                continue;
            };
            let elapsed = now.duration_since(session.updated_at).as_secs();
            match &session.state {
                ObservedSessionState::Idle if session.has_connection => {
                    snapshot.bound_threads.push(BoundThreadConnection {
                        id: format!("S{session_id:03}"),
                        thread_id: thread_id.clone(),
                        activity: ConnectionActivity::Idle,
                        idle_seconds: elapsed,
                        reclaim_policy: SessionReclaimPolicy::ThreadEnd,
                    });
                }
                ObservedSessionState::Active(activity) => {
                    snapshot.bound_threads.push(BoundThreadConnection {
                        id: format!("S{session_id:03}"),
                        thread_id: thread_id.clone(),
                        activity: *activity,
                        idle_seconds: 0,
                        reclaim_policy: SessionReclaimPolicy::ThreadEnd,
                    });
                }
                ObservedSessionState::Recovering(reason) => {
                    snapshot.transitions.push(ConnectionTransition {
                        id: format!("S{session_id:03}"),
                        label: "恢复绑定连接".to_owned(),
                        stage: "等待可用连接".to_owned(),
                        detail: reason.clone(),
                        elapsed_seconds: elapsed,
                    });
                }
                ObservedSessionState::Idle => {}
            }
        }

        let connecting = state
            .scopes
            .values()
            .map(|scope| scope.connecting)
            .sum::<usize>();
        if connecting > 0 {
            snapshot.transitions.push(ConnectionTransition {
                id: "POOL-CONNECT".to_owned(),
                label: "建立预热连接".to_owned(),
                stage: "连接中".to_owned(),
                detail: format!("{connecting} 条连接正在建立"),
                elapsed_seconds: 0,
            });
        }
        let probing = state
            .scopes
            .values()
            .map(|scope| scope.probing)
            .sum::<usize>();
        if probing > 0 {
            snapshot.transitions.push(ConnectionTransition {
                id: "POOL-PROBE".to_owned(),
                label: "检查预热连接".to_owned(),
                stage: "健康检查".to_owned(),
                detail: format!("{probing} 条连接正在检查"),
                elapsed_seconds: 0,
            });
        }

        snapshot
            .bound_threads
            .sort_by(|left, right| left.id.cmp(&right.id));
        snapshot.recent_closed = state
            .recent_closed
            .iter()
            .map(|closed| ClosedConnection {
                id: format!("C{:03}", closed.id),
                thread_id: closed.thread_id.clone(),
                reason: closed.reason.clone(),
                ago_seconds: now.duration_since(closed.closed_at).as_secs(),
                normal: closed.normal,
            })
            .collect();
        snapshot
    }
}
