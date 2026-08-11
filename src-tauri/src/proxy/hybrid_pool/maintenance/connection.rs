use std::{sync::Arc, time::Duration};

use axum::http::HeaderMap;
use url::Url;

use super::super::{
    HybridPool, HybridScope, PONG_TIMEOUT, PoolConnection, PoolInner, desired_connections,
    total_connections,
};
use crate::proxy::private_websocket;

const CONNECTION_RETRY_DELAY: Duration = Duration::from_secs(1);

#[derive(Clone)]
pub(super) struct ConnectionSpec {
    pub(super) target: Url,
    pub(super) headers: HeaderMap,
}

pub(super) fn spawn_connection(inner: Arc<PoolInner>, scope: HybridScope, spec: ConnectionSpec) {
    tokio::spawn(async move {
        let connected =
            private_websocket::connect_private(&spec.target, &spec.headers, &inner.tls_config)
                .await;
        let upstream = match connected {
            Ok(upstream) => upstream,
            Err(failure) => {
                let mut state = inner.state.lock().await;
                let (remove, retry) = if let Some(entry) = state.scopes.get_mut(&scope) {
                    entry.connecting = entry.connecting.saturating_sub(1);
                    entry.diagnostics.record_failure(failure);
                    (
                        entry.active_local == 0 && total_connections(entry) == 0,
                        entry.active_local > 0,
                    )
                } else {
                    (false, false)
                };
                if remove {
                    state.scopes.remove(&scope);
                }
                drop(state);
                if retry {
                    tokio::time::sleep(CONNECTION_RETRY_DELAY).await;
                    HybridPool { inner }.refill(&scope).await;
                }
                return;
            }
        };
        let mut upstream = Some(upstream);
        let accepted = {
            let mut state = inner.state.lock().await;
            let global_total = state.scopes.values().map(total_connections).sum::<usize>();
            let global_leased = state
                .scopes
                .values()
                .map(|candidate| candidate.leased.len())
                .sum::<usize>();
            let Some(entry) = state.scopes.get_mut(&scope) else {
                return;
            };
            entry.connecting = entry.connecting.saturating_sub(1);
            let keep = global_total <= desired_connections(global_leased)
                && total_connections(entry) < desired_connections(entry.leased.len());
            let connection_id = keep.then(|| state.allocate_connection_id());
            let Some(entry) = state.scopes.get_mut(&scope) else {
                return;
            };
            if let (Some(connection_id), Some(upstream)) = (connection_id, upstream.take()) {
                entry.idle.push(PoolConnection {
                    id: connection_id,
                    upstream,
                });
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
