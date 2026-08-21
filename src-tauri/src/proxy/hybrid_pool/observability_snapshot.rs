use tokio::time::Instant;

use super::{
    BoundThreadConnection, ClosedConnection, ConnectionActivity, ConnectionSnapshot,
    ConnectionTransition, HybridPool, ObservedSessionState, PoolState, SessionReclaimPolicy,
    recent_closed_visible,
};

struct PoolCounts {
    current_connections: usize,
    prewarm: usize,
}

impl HybridPool {
    pub(crate) async fn connection_snapshot(&self) -> ConnectionSnapshot {
        let state = self.inner.state.lock().await;
        let now = Instant::now();
        let counts = pool_counts(&state);
        let mut snapshot = ConnectionSnapshot {
            current_connections: counts.current_connections,
            prewarm: counts.prewarm,
            ..ConnectionSnapshot::default()
        };
        append_session_state(&mut snapshot, &state, now);
        append_pool_transitions(&mut snapshot, &state, now);
        snapshot.recent_closed = visible_closed(&state, now);
        drop(state);

        snapshot
            .bound_threads
            .sort_by(|left, right| left.id.cmp(&right.id));
        snapshot
    }
}

fn pool_counts(state: &PoolState) -> PoolCounts {
    state.scopes.values().fold(
        PoolCounts {
            current_connections: 0,
            prewarm: 0,
        },
        |counts, scope| PoolCounts {
            current_connections: counts
                .current_connections
                .saturating_add(scope.idle.len())
                .saturating_add(scope.leased.len())
                .saturating_add(scope.probing),
            prewarm: counts.prewarm.saturating_add(scope.idle.len()),
        },
    )
}

fn append_session_state(snapshot: &mut ConnectionSnapshot, state: &PoolState, now: Instant) {
    for scope in state.scopes.values() {
        for (session_id, lease) in &scope.leased {
            let Some(session) = state.sessions.get(session_id) else {
                continue;
            };
            let Some(thread_id) = session.thread_id.as_ref() else {
                continue;
            };
            let elapsed = now.duration_since(session.updated_at).as_secs();
            let activity = match &session.state {
                ObservedSessionState::Idle => ConnectionActivity::Idle,
                ObservedSessionState::Active(activity) => *activity,
                ObservedSessionState::Recovering(_) => continue,
            };
            snapshot.bound_threads.push(BoundThreadConnection {
                id: format!("S{:03}", lease.connection_id),
                thread_id: thread_id.clone(),
                activity,
                idle_seconds: if activity == ConnectionActivity::Idle {
                    elapsed
                } else {
                    0
                },
                reclaim_policy: SessionReclaimPolicy::ThreadEnd,
                upstream_trace: lease.server_trace.clone(),
                upstream_generation: lease.connection_id,
                upstream_ordinal: lease.ordinal,
                connection_age_seconds: now.duration_since(lease.metadata.created_at).as_secs(),
                last_probe_age_seconds: lease
                    .metadata
                    .last_probe_at
                    .map(|last_probe| now.duration_since(last_probe).as_secs()),
            });
        }
    }

    for (session_id, session) in &state.sessions {
        let ObservedSessionState::Recovering(reason) = &session.state else {
            continue;
        };
        let Some(thread_id) = session.thread_id.as_ref() else {
            continue;
        };
        snapshot.transitions.push(ConnectionTransition {
            id: format!("SESSION-{session_id:03}"),
            thread_id: Some(thread_id.clone()),
            connection_id: None,
            label: "恢复绑定连接".to_owned(),
            stage: "等待可用连接".to_owned(),
            detail: reason.clone(),
            elapsed_seconds: now.duration_since(session.updated_at).as_secs(),
        });
    }
}

fn append_pool_transitions(snapshot: &mut ConnectionSnapshot, state: &PoolState, now: Instant) {
    for (scope, entry) in &state.scopes {
        let fingerprint = scope.fingerprint(state.scopes.hasher());
        let mut waiting_count = 0usize;
        let mut waiting_since: Option<Instant> = None;
        for (session_id, session) in &state.sessions {
            if session.scope_fingerprint != fingerprint
                || entry.leased.contains_key(session_id)
                || matches!(&session.state, ObservedSessionState::Recovering(_))
            {
                continue;
            }
            waiting_count = waiting_count.saturating_add(1);
            waiting_since = Some(waiting_since.map_or(session.updated_at, |current| {
                current.min(session.updated_at)
            }));
        }
        let Some(waiting_since) = waiting_since else {
            continue;
        };
        let failure = entry
            .diagnostics
            .last_failure
            .map_or_else(String::new, |reason| {
                format!(
                    " · 最近握手失败：{reason}（累计 {} 次）",
                    entry.diagnostics.failed_attempts
                )
            });
        snapshot.transitions.push(ConnectionTransition {
            id: format!("POOL-BIND-{fingerprint}"),
            thread_id: None,
            connection_id: None,
            label: "分配会话连接".to_owned(),
            stage: "等待可用连接".to_owned(),
            detail: format!(
                "连接组 {fingerprint} · {waiting_count} 个会话等待 · 空白预热 {} · 建立中 {} · 检查中 {}{failure}",
                entry.idle.len(),
                entry.connecting,
                entry.probing,
            ),
            elapsed_seconds: now.duration_since(waiting_since).as_secs(),
        });
    }

    let connecting = state
        .scopes
        .values()
        .map(|scope| scope.connecting)
        .sum::<usize>();
    if connecting > 0 {
        snapshot.transitions.push(ConnectionTransition {
            id: "POOL-CONNECT".to_owned(),
            thread_id: None,
            connection_id: None,
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
            thread_id: None,
            connection_id: None,
            label: "检查预热连接".to_owned(),
            stage: "健康检查".to_owned(),
            detail: format!("{probing} 条连接正在检查"),
            elapsed_seconds: 0,
        });
    }
}

fn visible_closed(state: &PoolState, now: Instant) -> Vec<ClosedConnection> {
    state
        .recent_closed
        .iter()
        .filter_map(|closed| {
            let age = now.duration_since(closed.closed_at);
            recent_closed_visible(age).then(|| ClosedConnection {
                id: format!("C{:03}", closed.id),
                thread_id: closed.thread_id.clone(),
                connection_id: format!("S{:03}", closed.connection_id),
                reason: closed.reason.clone(),
                ago_seconds: age.as_secs(),
                normal: closed.normal,
            })
        })
        .collect()
}
