use std::{sync::Arc, time::Duration};

use axum::http::HeaderMap;
use url::Url;

use super::{
    HybridPool, HybridScope, MAX_POOL_CONNECTIONS, PONG_TIMEOUT, PoolInner, PrivateUpstream,
    desired_connections, probe_idle, total_connections,
};
use crate::proxy::private_websocket;

#[derive(Clone)]
struct ConnectionSpec {
    target: Url,
    headers: HeaderMap,
}

impl HybridPool {
    pub(super) async fn refill(&self, scope: &HybridScope) {
        let plan = {
            let mut state = self.inner.state.lock().await;
            let global_total = state.scopes.values().map(total_connections).sum::<usize>();
            let available = MAX_POOL_CONNECTIONS.saturating_sub(global_total);
            let Some(entry) = state.scopes.get(scope) else {
                return;
            };
            let current_total = total_connections(entry);
            if entry.active_local == 0 && current_total == 0 {
                state.scopes.remove(scope);
                return;
            }
            let needed = desired_connections(entry.active_local).saturating_sub(current_total);
            if needed == 0 {
                return;
            }
            let active_needed = entry.active_local.saturating_sub(current_total);
            let spec = ConnectionSpec {
                target: entry.target.clone(),
                headers: entry.headers.clone(),
            };
            let reclaim_target = needed.saturating_sub(available);
            let mut reclaimed = Vec::new();
            if entry.active_local > 0 && reclaim_target > 0 {
                for (candidate_scope, candidate) in &mut state.scopes {
                    if candidate_scope == scope || candidate.active_local > 0 {
                        continue;
                    }
                    let take = reclaim_target
                        .saturating_sub(reclaimed.len())
                        .min(candidate.idle.len());
                    reclaimed.extend(
                        candidate
                            .idle
                            .drain(candidate.idle.len().saturating_sub(take)..),
                    );
                    if reclaimed.len() == reclaim_target {
                        break;
                    }
                }
            }
            let active_reclaim_target =
                active_needed.saturating_sub(available.saturating_add(reclaimed.len()));
            if active_reclaim_target > 0 {
                let reclaimed_before_active = reclaimed.len();
                for (candidate_scope, candidate) in &mut state.scopes {
                    if candidate_scope == scope || candidate.active_local == 0 {
                        continue;
                    }
                    let spare = total_connections(candidate).saturating_sub(candidate.active_local);
                    let take = active_reclaim_target
                        .saturating_sub(reclaimed.len().saturating_sub(reclaimed_before_active))
                        .min(candidate.idle.len())
                        .min(spare);
                    reclaimed.extend(
                        candidate
                            .idle
                            .drain(candidate.idle.len().saturating_sub(take)..),
                    );
                    if reclaimed.len().saturating_sub(reclaimed_before_active)
                        == active_reclaim_target
                    {
                        break;
                    }
                }
            }
            state.scopes.retain(|_, candidate| {
                candidate.active_local > 0 || total_connections(candidate) > 0
            });
            let connecting = needed.min(available.saturating_add(reclaimed.len()));
            if connecting == 0 {
                return;
            }
            let Some(entry) = state.scopes.get_mut(scope) else {
                return;
            };
            entry.connecting = entry.connecting.saturating_add(connecting);
            let plan = (connecting, spec, reclaimed);
            drop(state);
            plan
        };
        let (connecting, spec, reclaimed) = plan;
        for _ in 0..reclaimed.len() {
            self.inner.metrics.record_websocket_closed();
        }
        drop(reclaimed);
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
        let upstream = {
            let mut state = self.inner.state.lock().await;
            let Some(entry) = state.scopes.get_mut(scope) else {
                return;
            };
            // ponytail: 唯一 reserve 必须保持可领取；仅在监控确认陈旧失败后再做 replace-before-probe。
            if entry.idle.len() <= 1 {
                return;
            }
            let upstream = entry.idle.remove(0);
            entry.probing = entry.probing.saturating_add(1);
            drop(state);
            upstream
        };
        let healthy = probe_idle(upstream, pong_timeout).await;
        let succeeded = healthy.is_some();
        {
            let mut state = self.inner.state.lock().await;
            let Some(entry) = state.scopes.get_mut(scope) else {
                return;
            };
            entry.probing = entry.probing.saturating_sub(1);
            if let Some(upstream) = healthy {
                entry.idle.push(upstream);
            }
            drop(state);
        }
        if succeeded {
            self.inner.ready.notify_waiters();
        } else {
            self.inner.metrics.record_websocket_closed();
        }
        self.refill(scope).await;
    }

    pub(super) async fn close_all(&self, upstreams: Vec<PrivateUpstream>) {
        for mut upstream in upstreams {
            let _ = tokio::time::timeout(PONG_TIMEOUT, upstream.close(None)).await;
            self.inner.metrics.record_websocket_closed();
        }
    }
}

fn spawn_connection(inner: Arc<PoolInner>, scope: HybridScope, spec: ConnectionSpec) {
    tokio::spawn(async move {
        let connected =
            private_websocket::connect_private(&spec.target, &spec.headers, &inner.tls_config)
                .await;
        let Some(upstream) = connected else {
            let mut state = inner.state.lock().await;
            let remove = if let Some(entry) = state.scopes.get_mut(&scope) {
                entry.connecting = entry.connecting.saturating_sub(1);
                entry.active_local == 0 && total_connections(entry) == 0
            } else {
                false
            };
            if remove {
                state.scopes.remove(&scope);
            }
            drop(state);
            return;
        };
        let mut upstream = Some(upstream);
        let accepted = {
            let mut state = inner.state.lock().await;
            let global_total = state.scopes.values().map(total_connections).sum::<usize>();
            let Some(entry) = state.scopes.get_mut(&scope) else {
                return;
            };
            entry.connecting = entry.connecting.saturating_sub(1);
            let keep = global_total <= MAX_POOL_CONNECTIONS
                && total_connections(entry) < desired_connections(entry.active_local);
            if keep {
                if let Some(upstream) = upstream.take() {
                    entry.idle.push(upstream);
                }
                entry.initialized = true;
            }
            drop(state);
            keep
        };
        if accepted {
            inner.metrics.record_websocket_connected();
            inner.ready.notify_waiters();
        } else if let Some(mut upstream) = upstream {
            let _ = tokio::time::timeout(PONG_TIMEOUT, upstream.close(None)).await;
        }
    });
}
