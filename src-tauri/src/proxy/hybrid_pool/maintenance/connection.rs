use std::{sync::Arc, time::Duration};

use axum::http::HeaderMap;
use url::Url;

use super::super::{
    HybridPool, HybridScope, PONG_TIMEOUT, PoolConnection, PoolInner, total_connections,
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
        let (upstream, server_trace) = match connected {
            Ok(connection) => connection,
            Err(failure) => {
                let mut state = inner.state.lock().await;
                let (remove, retry) = if let Some(entry) = state.scopes.get_mut(&scope) {
                    entry.connecting = entry.connecting.saturating_sub(1);
                    entry.record_failure(failure);
                    (
                        entry.active_local == 0 && total_connections(entry) == 0,
                        !entry.waiting.is_empty(),
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
            let Some(entry) = state.scopes.get_mut(&scope) else {
                return;
            };
            entry.connecting = entry.connecting.saturating_sub(1);
            let keep = entry.waiting.len() > entry.idle.len() + entry.probing;
            let connection_id = keep.then(|| state.allocate_connection_id());
            let Some(entry) = state.scopes.get_mut(&scope) else {
                return;
            };
            if let Some(connection_id) = connection_id
                && let Some(upstream) = upstream.take()
            {
                entry.idle.push(PoolConnection {
                    id: connection_id,
                    upstream,
                    server_trace,
                    ordinal: 0,
                    metadata: super::super::ConnectionMetadata::fresh(),
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
            let _ = tokio::time::timeout(
                PONG_TIMEOUT,
                upstream.close(Some(tokio_tungstenite::tungstenite::protocol::CloseFrame {
                    code:
                        tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Normal,
                    reason: "".into(),
                })),
            )
            .await;
        }
    });
}
