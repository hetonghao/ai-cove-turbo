use std::{collections::HashMap, fmt, sync::Arc, time::Duration};

use axum::http::HeaderMap;
use tokio::sync::{Mutex, Notify};
use tokio::time::Instant;
use url::Url;

use super::{Metrics, private_websocket};
use private_websocket::{PrivateTlsConfig, PrivateUpstream};

mod expiration;
mod maintenance;
pub(super) use maintenance::probe_idle;

#[cfg(test)]
#[path = "hybrid_pool_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "hybrid_pool_capacity_tests.rs"]
mod capacity_tests;

const MAX_POOL_CONNECTIONS: usize = 32;
const IDLE_CONNECTION_TIMEOUT: Duration = Duration::from_secs(60);
pub(super) const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(20);
pub(super) const PONG_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Eq, Hash, PartialEq)]
pub(super) struct HybridScope {
    target: String,
    headers: Vec<(String, Vec<u8>)>,
}

impl HybridScope {
    pub(super) fn new(target: &Url, client_headers: &HeaderMap) -> Self {
        let hop_by_hop = super::hop_by_hop_headers(client_headers);
        let mut headers = client_headers
            .iter()
            .filter(|(name, _)| {
                !hop_by_hop.contains(*name) && !private_websocket::is_client_handshake_header(name)
            })
            .map(|(name, value)| (name.as_str().to_owned(), value.as_bytes().to_vec()))
            .collect::<Vec<_>>();
        headers.sort_unstable();
        Self {
            target: target.as_str().to_owned(),
            headers,
        }
    }
}

impl fmt::Debug for HybridScope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HybridScope")
            .field("header_count", &self.headers.len())
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub(super) struct HybridPool {
    inner: Arc<PoolInner>,
}

struct PoolInner {
    state: Mutex<PoolState>,
    ready: Notify,
    tls_config: PrivateTlsConfig,
    metrics: Arc<Metrics>,
}

#[derive(Default)]
struct PoolState {
    scopes: HashMap<HybridScope, ScopeState>,
    dormant: HashMap<HybridScope, Instant>,
}

struct ScopeState {
    target: Url,
    headers: HeaderMap,
    initialized: bool,
    active_local: usize,
    leased: usize,
    connecting: usize,
    probing: usize,
    idle: Vec<PrivateUpstream>,
}

impl fmt::Debug for HybridPool {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("HybridPool").finish_non_exhaustive()
    }
}

impl HybridPool {
    pub(super) fn new(tls_config: PrivateTlsConfig, metrics: Arc<Metrics>) -> Self {
        let pool = Self {
            inner: Arc::new(PoolInner {
                state: Mutex::new(PoolState::default()),
                ready: Notify::new(),
                tls_config,
                metrics,
            }),
        };
        pool.spawn_maintenance();
        pool
    }

    pub(super) async fn register(&self, scope: &HybridScope, target: Url, headers: HeaderMap) {
        {
            let mut state = self.inner.state.lock().await;
            state.dormant.remove(scope);
            let entry = state
                .scopes
                .entry(scope.clone())
                .or_insert_with(|| ScopeState {
                    target,
                    headers,
                    initialized: false,
                    active_local: 0,
                    leased: 0,
                    connecting: 0,
                    probing: 0,
                    idle: Vec::new(),
                });
            entry.active_local = entry.active_local.saturating_add(1);
            drop(state);
        }
        self.refill(scope).await;
    }

    pub(super) async fn unregister(&self, scope: &HybridScope) {
        let (to_close, deadline) = {
            let mut state = self.inner.state.lock().await;
            let Some(entry) = state.scopes.get_mut(scope) else {
                return;
            };
            entry.active_local = entry.active_local.saturating_sub(1);
            let desired = desired_connections(entry.active_local);
            let excess = total_connections(entry).saturating_sub(desired);
            let close_count = excess.min(entry.idle.len());
            let to_close = entry
                .idle
                .drain(entry.idle.len().saturating_sub(close_count)..)
                .collect::<Vec<_>>();
            let deadline =
                (entry.active_local == 0).then(|| Instant::now() + IDLE_CONNECTION_TIMEOUT);
            if let Some(deadline) = deadline {
                state.dormant.insert(scope.clone(), deadline);
            }
            drop(state);
            (to_close, deadline)
        };
        self.close_all(to_close).await;
        if let Some(deadline) = deadline {
            self.schedule_dormant_expiration(scope.clone(), deadline);
        }
    }

    pub(super) async fn checkout(&self, scope: &HybridScope) -> Option<PrivateUpstream> {
        let upstream = {
            let mut state = self.inner.state.lock().await;
            let entry = state.scopes.get_mut(scope)?;
            let upstream = entry.idle.pop();
            if upstream.is_some() {
                entry.leased = entry.leased.saturating_add(1);
            }
            drop(state);
            upstream
        };
        if upstream.is_none() {
            self.refill(scope).await;
        }
        upstream
    }

    pub(super) async fn checkout_wait(
        &self,
        scope: &HybridScope,
        wait: Duration,
    ) -> Option<PrivateUpstream> {
        let checkout = async {
            loop {
                let notified = self.inner.ready.notified();
                if let Some(upstream) = self.checkout(scope).await {
                    return upstream;
                }
                notified.await;
            }
        };
        tokio::time::timeout(wait, checkout).await.ok()
    }

    pub(super) async fn has_initialized(&self, scope: &HybridScope) -> bool {
        self.inner
            .state
            .lock()
            .await
            .scopes
            .get(scope)
            .is_some_and(|entry| entry.initialized)
    }

    pub(super) async fn release_session_connection(
        &self,
        scope: &HybridScope,
        upstream: Option<PrivateUpstream>,
    ) {
        {
            let mut state = self.inner.state.lock().await;
            if let Some(entry) = state.scopes.get_mut(scope) {
                entry.leased = entry.leased.saturating_sub(1);
            }
            drop(state);
        }
        if let Some(upstream) = upstream {
            self.close_all(vec![upstream]).await;
        }
    }

    pub(super) async fn discard(&self, scope: &HybridScope) {
        {
            let mut state = self.inner.state.lock().await;
            let Some(entry) = state.scopes.get_mut(scope) else {
                return;
            };
            entry.leased = entry.leased.saturating_sub(1);
            drop(state);
        }
        self.refill(scope).await;
    }
}

const fn desired_connections(active_local: usize) -> usize {
    let desired = active_local.saturating_add(1);
    if desired > MAX_POOL_CONNECTIONS {
        MAX_POOL_CONNECTIONS
    } else {
        desired
    }
}

fn total_connections(scope: &ScopeState) -> usize {
    scope
        .idle
        .len()
        .saturating_add(scope.leased)
        .saturating_add(scope.connecting)
        .saturating_add(scope.probing)
}
