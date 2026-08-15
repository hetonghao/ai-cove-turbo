use std::{collections::HashMap, fmt, sync::Arc, time::Duration};

use axum::http::HeaderMap;
use tokio::sync::{Mutex, Notify};
use url::Url;

use super::{
    Metrics,
    private_websocket::{PrivateTlsConfig, PrivateUpstream},
};

mod diagnostics;
mod maintenance;
mod observability;
mod probe;
mod scope;
use diagnostics::ScopeDiagnostics;
pub(crate) use observability::ConnectionSnapshot;
use observability::{ClosedRecord, ObservedSession};
pub(super) use observability::{ConnectionActivity, ConnectionObservation, LeaseRetirement};
pub(super) use probe::probe_idle;
pub(super) use scope::HybridScope;
use scope::blank_connection_headers;

#[cfg(test)]
#[path = "hybrid_pool_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "hybrid_pool_maintenance_tests.rs"]
mod maintenance_tests;

#[cfg(test)]
#[path = "hybrid_pool_capacity_tests.rs"]
mod capacity_tests;

#[cfg(test)]
#[path = "hybrid_scope_tests.rs"]
mod scope_tests;

#[cfg(test)]
#[path = "hybrid_pool/resource_truth_tests.rs"]
mod resource_truth_tests;

const MAX_POOL_CONNECTIONS: usize = 100;
const MAX_PREWARM_CONNECTIONS: usize = 6;
const MIN_PREWARM_CONNECTIONS: usize = 1;
const ACTIVE_CONNECTIONS_PER_PREWARM_REDUCTION: usize = 5;
pub(super) const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(20);
pub(super) const PONG_TIMEOUT: Duration = Duration::from_secs(10);
const HANDOFF_WAIT: Duration = Duration::from_millis(250);
#[cfg(not(test))]
const HANDOFF_WINDOW: Duration = Duration::from_secs(60);
#[cfg(test)]
const HANDOFF_WINDOW: Duration = Duration::from_millis(500);

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
    next_session_id: u64,
    next_connection_id: u64,
    sessions: HashMap<u64, ObservedSession>,
    handoffs: Vec<ParkedConnection>,
    next_closed_id: u64,
    recent_closed: std::collections::VecDeque<ClosedRecord>,
}

struct ScopeState {
    target: Url,
    headers: HeaderMap,
    diagnostics: ScopeDiagnostics,
    initialized: bool,
    active_local: usize,
    leased: HashMap<u64, ConnectionLease>,
    connecting: usize,
    probing: usize,
    idle: Vec<PoolConnection>,
}

struct PoolConnection {
    id: u64,
    upstream: PrivateUpstream,
    server_trace: Option<String>,
    ordinal: u64,
}

struct ConnectionLease {
    connection_id: u64,
    server_trace: Option<String>,
    ordinal: u64,
}

struct ParkedConnection {
    scope: HybridScope,
    session_id: u64,
    thread_id: String,
    response_id: String,
    connection_id: u64,
    server_trace: Option<String>,
    ordinal: u64,
    upstream: PrivateUpstream,
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

    pub(super) async fn register(
        &self,
        scope: &HybridScope,
        target: Url,
        headers: HeaderMap,
    ) -> u64 {
        let headers = blank_connection_headers(&headers);
        let session_id = {
            let mut state = self.inner.state.lock().await;
            let scope_fingerprint = scope.fingerprint(state.scopes.hasher());
            let entry = state
                .scopes
                .entry(scope.clone())
                .or_insert_with(|| ScopeState {
                    target,
                    headers,
                    diagnostics: ScopeDiagnostics::default(),
                    initialized: false,
                    active_local: 0,
                    leased: HashMap::new(),
                    connecting: 0,
                    probing: 0,
                    idle: Vec::new(),
                });
            entry.active_local = entry.active_local.saturating_add(1);
            state.register_session(scope_fingerprint)
        };
        self.refill(scope).await;
        session_id
    }

    pub(super) async fn unregister(&self, scope: &HybridScope, session_id: u64) {
        let to_close = {
            let mut state = self.inner.state.lock().await;
            let Some(entry) = state.scopes.get_mut(scope) else {
                state.remove_session(session_id, None);
                return;
            };
            let (connection_id, to_close) = {
                entry.active_local = entry.active_local.saturating_sub(1);
                let connection_id = entry
                    .leased
                    .remove(&session_id)
                    .map(|lease| lease.connection_id);
                let desired = desired_connections(entry.leased.len());
                let excess = total_connections(entry).saturating_sub(desired);
                let close_count = excess.min(entry.idle.len());
                let to_close = entry
                    .idle
                    .drain(entry.idle.len().saturating_sub(close_count)..)
                    .collect::<Vec<_>>();
                (connection_id, to_close)
            };
            state.remove_session(session_id, connection_id);
            for connection in &to_close {
                state.push_closed(connection.id, None, "连接池容量回收".to_owned(), true);
            }
            state.scopes.retain(|_, candidate| {
                candidate.active_local > 0 || total_connections(candidate) > 0
            });
            drop(state);
            to_close
        };
        self.close_pool_connections_detached(to_close);
    }

    pub(super) async fn checkout(
        &self,
        scope: &HybridScope,
        session_id: u64,
    ) -> Option<PrivateUpstream> {
        let upstream = {
            let mut state = self.inner.state.lock().await;
            if !state.sessions.contains_key(&session_id) {
                return None;
            }
            let entry = state.scopes.get_mut(scope)?;
            if entry.leased.contains_key(&session_id) {
                return None;
            }
            let connection = entry.idle.pop();
            let upstream = connection.map(|connection| {
                entry.leased.insert(
                    session_id,
                    ConnectionLease {
                        connection_id: connection.id,
                        server_trace: connection.server_trace,
                        ordinal: connection.ordinal,
                    },
                );
                connection.upstream
            });
            drop(state);
            upstream
        };
        if upstream.is_some() {
            let pool = self.clone();
            let scope = scope.clone();
            tokio::spawn(async move {
                pool.refill(&scope).await;
            });
        } else {
            self.refill(scope).await;
        }
        upstream
    }

    pub(super) async fn checkout_wait(
        &self,
        scope: &HybridScope,
        session_id: u64,
        wait: Duration,
    ) -> Option<PrivateUpstream> {
        tokio::time::timeout(wait, self.checkout_ready(scope, session_id))
            .await
            .ok()
    }

    pub(super) async fn checkout_ready(
        &self,
        scope: &HybridScope,
        session_id: u64,
    ) -> PrivateUpstream {
        loop {
            let notified = self.inner.ready.notified();
            if let Some(upstream) = self.checkout(scope, session_id).await {
                return upstream;
            }
            notified.await;
        }
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
        session_id: u64,
        upstream: Option<PrivateUpstream>,
    ) {
        {
            let mut state = self.inner.state.lock().await;
            let connection_id = state
                .scopes
                .get_mut(scope)
                .and_then(|entry| entry.leased.remove(&session_id))
                .map(|lease| lease.connection_id);
            state.release_session(session_id, connection_id);
            drop(state);
        }
        if let Some(upstream) = upstream {
            self.close_all(vec![upstream]).await;
        }
    }

    pub(super) async fn park_session_connection(
        &self,
        scope: &HybridScope,
        session_id: u64,
        thread_id: String,
        response_id: String,
        upstream: PrivateUpstream,
    ) -> Result<(), PrivateUpstream> {
        let connection_id = {
            let mut state = self.inner.state.lock().await;
            if state
                .handoffs
                .iter()
                .any(|handoff| handoff.session_id == session_id)
            {
                return Err(upstream);
            }
            let Some(entry) = state.scopes.get_mut(scope) else {
                return Err(upstream);
            };
            let Some(lease) = entry.leased.remove(&session_id) else {
                return Err(upstream);
            };
            let connection_id = lease.connection_id;
            entry.active_local = entry.active_local.saturating_sub(1);
            state.release_session(session_id, None);
            state.handoffs.push(ParkedConnection {
                scope: scope.clone(),
                session_id,
                thread_id,
                response_id,
                connection_id,
                server_trace: lease.server_trace,
                ordinal: lease.ordinal,
                upstream,
            });
            connection_id
        };
        self.inner.ready.notify_waiters();
        let inner = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            tokio::time::sleep(HANDOFF_WINDOW).await;
            if let Some(inner) = inner.upgrade() {
                Self { inner }
                    .expire_handoff(session_id, connection_id)
                    .await;
            }
        });
        Ok(())
    }

    pub(super) async fn checkout_handoff_wait(
        &self,
        scope: &HybridScope,
        session_id: u64,
        thread_id: &str,
        response_id: &str,
    ) -> Option<PrivateUpstream> {
        let checkout = async {
            loop {
                let notified = self.inner.ready.notified();
                let upstream = {
                    let mut state = self.inner.state.lock().await;
                    let index = state.handoffs.iter().position(|handoff| {
                        &handoff.scope == scope
                            && handoff.thread_id == thread_id
                            && handoff.response_id == response_id
                    });
                    let upstream = index.and_then(|index| {
                        if !state.sessions.contains_key(&session_id) {
                            return None;
                        }
                        let parked = state.handoffs.swap_remove(index);
                        let entry = state.scopes.get_mut(scope)?;
                        entry.leased.remove(&parked.session_id);
                        entry.leased.insert(
                            session_id,
                            ConnectionLease {
                                connection_id: parked.connection_id,
                                server_trace: parked.server_trace,
                                ordinal: parked.ordinal,
                            },
                        );
                        state.remove_session(parked.session_id, None);
                        Some(parked.upstream)
                    });
                    drop(state);
                    upstream
                };
                if upstream.is_some() {
                    return upstream;
                }
                notified.await;
            }
        };
        tokio::time::timeout(HANDOFF_WAIT, checkout)
            .await
            .ok()
            .flatten()
    }

    async fn expire_handoff(&self, session_id: u64, connection_id: u64) {
        let expired = {
            let mut state = self.inner.state.lock().await;
            let Some(index) = state.handoffs.iter().position(|handoff| {
                handoff.session_id == session_id && handoff.connection_id == connection_id
            }) else {
                return;
            };
            let parked = state.handoffs.swap_remove(index);
            if let Some(entry) = state.scopes.get_mut(&parked.scope) {
                entry.leased.remove(&session_id);
            }
            state.remove_session(session_id, Some(connection_id));
            state.scopes.retain(|_, candidate| {
                candidate.active_local > 0 || total_connections(candidate) > 0
            });
            drop(state);
            (parked.scope, parked.upstream)
        };
        self.close_all(vec![expired.1]).await;
        self.refill(&expired.0).await;
    }

    pub(super) async fn discard(
        &self,
        scope: &HybridScope,
        session_id: u64,
        retirement: LeaseRetirement,
    ) {
        {
            let mut state = self.inner.state.lock().await;
            let connection_id = state
                .scopes
                .get_mut(scope)
                .and_then(|entry| entry.leased.remove(&session_id))
                .map(|lease| lease.connection_id);
            state.retire_session(session_id, connection_id, retirement);
            drop(state);
        }
        self.refill(scope).await;
    }
}

const fn desired_connections(leased_connections: usize) -> usize {
    let reduced = MAX_PREWARM_CONNECTIONS
        .saturating_sub(leased_connections / ACTIVE_CONNECTIONS_PER_PREWARM_REDUCTION);
    let reserve = if reduced < MIN_PREWARM_CONNECTIONS {
        MIN_PREWARM_CONNECTIONS
    } else {
        reduced
    };
    let desired = leased_connections.saturating_add(reserve);
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
        .saturating_add(scope.leased.len())
        .saturating_add(scope.connecting)
        .saturating_add(scope.probing)
}
