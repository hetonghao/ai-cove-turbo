use std::{sync::Arc, time::Duration};

use futures_util::{StreamExt, stream};
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, frame::coding::CloseCode};

use super::{
    HybridPool, HybridScope, PONG_TIMEOUT, PoolConnection, PrivateUpstream, desired_connections,
    probe::ProbeFailure, probe_idle_detailed, total_connections,
};

mod connection;
use connection::{ConnectionSpec, spawn_connection};

const MAINTENANCE_CONCURRENCY: usize = 4;

impl HybridPool {
    pub(super) async fn refill(&self, scope: &HybridScope) {
        let mut state = self.inner.state.lock().await;
        let global_total =
            state.scopes.values().map(total_connections).sum::<usize>() + state.handoffs.len();
        let Some(entry) = state.scopes.get_mut(scope) else {
            return;
        };
        let desired = desired_connections(entry.leased.len() + entry.waiting.len());
        let excess = total_connections(entry)
            .saturating_sub(desired)
            .min(entry.idle.len());
        let reclaimed = entry.idle.drain(..excess).collect::<Vec<_>>();
        let connecting = desired
            .saturating_sub(total_connections(entry))
            .min(super::MAX_POOL_CONNECTIONS.saturating_sub(global_total.saturating_sub(excess)));
        let spec = ConnectionSpec {
            target: entry.target.clone(),
            headers: entry.headers.clone(),
        };
        entry.add_connecting(connecting);
        for connection in &reclaimed {
            state.push_closed(
                connection.id,
                None,
                "没有等待 WS 的请求，释放空闲连接".to_owned(),
                true,
            );
        }
        drop(state);
        self.close_pool_connections_detached(reclaimed);
        for _ in 0..connecting {
            spawn_connection(Arc::clone(&self.inner), scope.clone(), spec.clone());
        }
    }

    pub(super) fn spawn_maintenance(&self) {
        let inner = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            let mut cursor = 0usize;
            loop {
                tokio::time::sleep(super::KEEPALIVE_INTERVAL).await;
                let Some(inner) = inner.upgrade() else {
                    return;
                };
                Self { inner }.maintain_all(PONG_TIMEOUT, &mut cursor).await;
            }
        });
    }

    async fn maintain_all(&self, pong_timeout: Duration, cursor: &mut usize) {
        let started = tokio::time::Instant::now();
        let mut scopes = self
            .inner
            .state
            .lock()
            .await
            .scopes
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let slow = if scopes.is_empty() {
            false
        } else {
            scopes.sort_unstable();
            let offset = *cursor % scopes.len();
            scopes.rotate_left(offset);
            *cursor = cursor.saturating_add(1) % scopes.len();
            stream::iter(scopes)
                .map(|scope| async move { self.maintain_once(&scope, pong_timeout).await })
                .buffer_unordered(MAINTENANCE_CONCURRENCY)
                .fold(false, |slow, probe_timed_out| async move {
                    slow || probe_timed_out
                })
                .await
        };
        self.inner
            .metrics
            .record_maintenance_cycle(started.elapsed(), slow);
    }

    pub(super) async fn maintain_once(&self, scope: &HybridScope, pong_timeout: Duration) -> bool {
        let connection = {
            let mut state = self.inner.state.lock().await;
            let Some(entry) = state.scopes.get_mut(scope) else {
                return false;
            };
            if entry.idle_len() == 0 {
                return false;
            }
            let connection = entry.idle.remove(0);
            entry.add_probing();
            drop(state);
            connection
        };
        let mut probe_guard = MaintenanceProbeGuard::new(self.clone(), scope.clone(), connection);
        let probe = {
            let Some(upstream) = probe_guard.upstream_mut() else {
                return false;
            };
            probe_idle_detailed(upstream, pong_timeout).await
        };
        let mut state = self.inner.state.lock().await;
        let Some(entry) = state.scopes.get_mut(scope) else {
            drop(state);
            return false;
        };
        entry.remove_probing();
        let Some(connection) = probe_guard.take_connection() else {
            drop(state);
            return false;
        };
        let timed_out = matches!(probe, Err(ProbeFailure::Timeout));
        match probe {
            Ok(()) => {
                probe_guard.finish(false, false);
                entry.idle.push(PoolConnection {
                    id: connection.id,
                    upstream: connection.upstream,
                    server_trace: connection.server_trace,
                    ordinal: connection.ordinal,
                    metadata: connection.metadata.verified_now(),
                });
                drop(state);
                self.inner.ready.notify_waiters();
            }
            Err(failure) => {
                probe_guard.finish(true, matches!(failure, ProbeFailure::Timeout));
                state.push_closed(
                    connection.id,
                    None,
                    format!("连接池健康检查失败：{}", failure.reason()),
                    false,
                );
                drop(state);
                self.close_detached(vec![connection.upstream]);
            }
        }
        self.refill(scope).await;
        timed_out
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

struct MaintenanceProbeGuard {
    pool: HybridPool,
    scope: HybridScope,
    connection: Option<PoolConnection>,
    outcome: Option<(bool, bool)>,
}

impl MaintenanceProbeGuard {
    fn new(pool: HybridPool, scope: HybridScope, connection: PoolConnection) -> Self {
        pool.inner.metrics.record_maintenance_probe_started();
        Self {
            pool,
            scope,
            connection: Some(connection),
            outcome: None,
        }
    }

    fn upstream_mut(&mut self) -> Option<&mut PrivateUpstream> {
        self.connection
            .as_mut()
            .map(|connection| &mut connection.upstream)
    }

    const fn take_connection(&mut self) -> Option<PoolConnection> {
        self.connection.take()
    }

    const fn finish(&mut self, failed: bool, timed_out: bool) {
        self.outcome = Some((failed, timed_out));
    }
}

impl Drop for MaintenanceProbeGuard {
    fn drop(&mut self) {
        let (failed, timed_out) = self.outcome.unwrap_or((true, false));
        self.pool
            .inner
            .metrics
            .record_maintenance_probe_completed(failed, timed_out);
        let Some(connection) = self.connection.take() else {
            return;
        };
        let pool = self.pool.clone();
        let scope = self.scope.clone();
        tokio::spawn(async move {
            let mut state = pool.inner.state.lock().await;
            if let Some(entry) = state.scopes.get_mut(&scope) {
                entry.remove_probing();
                state.push_closed(
                    connection.id,
                    None,
                    "连接池维护预检被取消".to_owned(),
                    false,
                );
            }
            drop(state);
            pool.close_all(vec![connection.upstream]).await;
            pool.refill(&scope).await;
        });
    }
}
