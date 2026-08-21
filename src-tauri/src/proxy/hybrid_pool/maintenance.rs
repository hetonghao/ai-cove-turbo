use std::{sync::Arc, time::Duration};

use tokio::time::Instant;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, frame::coding::CloseCode};

use super::{
    HybridPool, HybridScope, PONG_TIMEOUT, PoolConnection, PoolState, PrivateUpstream,
    desired_connections, probe_idle_detailed, total_connections,
};

mod connection;
use connection::{ConnectionSpec, spawn_connection};

fn reclaim_idle(state: &mut PoolState, scope: &HybridScope, count: usize) -> Vec<PoolConnection> {
    let mut reclaimed = Vec::new();
    for candidate_is_active in [false, true] {
        for (candidate_scope, candidate) in &mut state.scopes {
            if candidate_scope == scope || (candidate.active_local > 0) != candidate_is_active {
                continue;
            }
            let take = count
                .saturating_sub(reclaimed.len())
                .min(candidate.idle.len());
            reclaimed.extend(
                candidate
                    .idle
                    .drain(candidate.idle.len().saturating_sub(take)..),
            );
            if reclaimed.len() == count {
                return reclaimed;
            }
        }
    }
    if let Some(candidate) = state.scopes.get_mut(scope) {
        let take = count
            .saturating_sub(reclaimed.len())
            .min(candidate.idle.len());
        reclaimed.extend(
            candidate
                .idle
                .drain(candidate.idle.len().saturating_sub(take)..),
        );
    }
    reclaimed
}

impl HybridPool {
    pub(super) async fn refill(&self, scope: &HybridScope) {
        let plan = {
            let mut state = self.inner.state.lock().await;
            let global_total = state.scopes.values().map(total_connections).sum::<usize>();
            let global_leased = state
                .scopes
                .values()
                .map(|candidate| candidate.leased.len())
                .sum::<usize>();
            let global_desired = desired_connections(global_leased);
            let available = global_desired.saturating_sub(global_total);
            let excess = global_total.saturating_sub(global_desired);
            let inactive_idle = state
                .scopes
                .iter()
                .filter(|(candidate_scope, candidate)| {
                    *candidate_scope != scope && candidate.active_local == 0
                })
                .map(|(_, candidate)| candidate.idle.len())
                .sum::<usize>();
            let Some(entry) = state.scopes.get(scope) else {
                return;
            };
            let current_total = total_connections(entry);
            if entry.active_local == 0 && current_total == 0 {
                state.scopes.remove(scope);
                return;
            }
            let active_local = entry.active_local;
            let leased_local = entry.leased.len();
            let ready_needed = usize::from(
                active_local > leased_local
                    && entry.idle.is_empty()
                    && entry.connecting == 0
                    && entry.probing == 0,
            );
            let needed = desired_connections(leased_local)
                .saturating_sub(current_total)
                .min(available.saturating_add(inactive_idle).max(ready_needed));
            if needed == 0 && excess == 0 {
                return;
            }
            let spec = ConnectionSpec {
                target: entry.target.clone(),
                headers: entry.headers.clone(),
            };
            let reclaim_target = excess.saturating_add(needed.saturating_sub(available));
            let reclaimed = reclaim_idle(&mut state, scope, reclaim_target);
            for connection in &reclaimed {
                state.push_closed(connection.id, None, "连接池容量回收".to_owned(), true);
            }
            state.scopes.retain(|_, candidate| {
                candidate.active_local > 0 || total_connections(candidate) > 0
            });
            let remaining_total = global_total.saturating_sub(reclaimed.len());
            let connecting = needed.min(global_desired.saturating_sub(remaining_total));
            if let Some(entry) = state.scopes.get_mut(scope) {
                entry.connecting = entry.connecting.saturating_add(connecting);
            }
            let plan = (connecting, spec, reclaimed);
            drop(state);
            plan
        };
        let (connecting, spec, reclaimed) = plan;
        self.close_pool_connections_detached(reclaimed);
        for _ in 0..connecting {
            spawn_connection(Arc::clone(&self.inner), scope.clone(), spec.clone());
        }
    }

    pub(super) fn spawn_maintenance(&self) {
        let inner = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(super::KEEPALIVE_INTERVAL).await;
                let Some(inner) = inner.upgrade() else {
                    return;
                };
                Self { inner }.maintain_all(PONG_TIMEOUT).await;
            }
        });
    }

    async fn maintain_all(&self, pong_timeout: Duration) {
        let scopes = {
            self.inner
                .state
                .lock()
                .await
                .scopes
                .keys()
                .cloned()
                .collect::<Vec<_>>()
        };
        for scope in scopes {
            self.maintain_once(&scope, pong_timeout).await;
        }
    }

    pub(super) async fn maintain_once(&self, scope: &HybridScope, pong_timeout: Duration) {
        let connection = {
            let mut state = self.inner.state.lock().await;
            let Some(entry) = state.scopes.get_mut(scope) else {
                return;
            };
            if entry.idle.is_empty() {
                return;
            }
            let connection = entry.idle.remove(0);
            entry.probing = entry.probing.saturating_add(1);
            drop(state);
            connection
        };
        let connection_id = connection.id;
        let mut upstream = connection.upstream;
        let (succeeded, failure_reason) =
            match probe_idle_detailed(&mut upstream, pong_timeout).await {
                Ok(()) => (true, None),
                Err(failure) => (false, Some(failure.reason())),
            };
        let failed_upstream = {
            let mut state = self.inner.state.lock().await;
            let Some(entry) = state.scopes.get_mut(scope) else {
                drop(state);
                self.close_detached(vec![upstream]);
                return;
            };
            entry.probing = entry.probing.saturating_sub(1);
            if succeeded {
                entry.idle.push(PoolConnection {
                    id: connection_id,
                    upstream,
                    server_trace: connection.server_trace,
                    ordinal: connection.ordinal,
                    last_probe_at: Some(Instant::now()),
                });
                drop(state);
                None
            } else {
                state.push_closed(
                    connection_id,
                    None,
                    format!(
                        "连接池健康检查失败：{}",
                        failure_reason.unwrap_or("unknown")
                    ),
                    false,
                );
                drop(state);
                Some(upstream)
            }
        };
        if failed_upstream.is_none() {
            self.inner.ready.notify_waiters();
        } else if let Some(upstream) = failed_upstream {
            self.close_detached(vec![upstream]);
        }
        self.refill(scope).await;
    }

    pub(super) fn close_detached(&self, upstreams: Vec<PrivateUpstream>) {
        if upstreams.is_empty() {
            return;
        }
        let pool = self.clone();
        tokio::spawn(async move {
            pool.close_all(upstreams).await;
        });
    }

    pub(super) fn close_pool_connections_detached(&self, connections: Vec<PoolConnection>) {
        self.close_detached(
            connections
                .into_iter()
                .map(|connection| connection.upstream)
                .collect(),
        );
    }

    pub(in crate::proxy) async fn close_all(&self, upstreams: Vec<PrivateUpstream>) {
        for mut upstream in upstreams {
            let close = CloseFrame {
                code: CloseCode::Normal,
                reason: "".into(),
            };
            let _ = tokio::time::timeout(PONG_TIMEOUT, upstream.close(Some(close))).await;
            self.inner.metrics.record_websocket_closed();
        }
    }
}
