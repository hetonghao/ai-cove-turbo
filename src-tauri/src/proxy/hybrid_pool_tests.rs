use std::{io, sync::Arc, time::Duration};

use futures_util::{SinkExt, StreamExt};
use rustls::RootCertStore;
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{Message, protocol::Role},
};

use super::*;

#[tokio::test]
async fn idle_private_websocket_remains_available_after_pong() -> io::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let address = listener.local_addr()?;
    let client_stream = TcpStream::connect(address).await?;
    let (server_stream, _) = listener.accept().await?;
    let client =
        WebSocketStream::from_raw_socket(MaybeTlsStream::Plain(client_stream), Role::Client, None)
            .await;
    let mut server = WebSocketStream::from_raw_socket(server_stream, Role::Server, None).await;
    let server_task = tokio::spawn(async move {
        let Some(Ok(Message::Ping(payload))) = server.next().await else {
            return Err(io::Error::other("keepalive ping missing"));
        };
        server
            .send(Message::Pong(payload))
            .await
            .map_err(io::Error::other)
    });

    let healthy = probe_idle(client, Duration::from_millis(100)).await;

    assert!(healthy.is_some());
    server_task.await.map_err(io::Error::other)??;
    Ok(())
}

#[tokio::test]
async fn idle_private_websocket_is_removed_after_pong_timeout() -> io::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let address = listener.local_addr()?;
    let client_stream = TcpStream::connect(address).await?;
    let (server_stream, _) = listener.accept().await?;
    let client =
        WebSocketStream::from_raw_socket(MaybeTlsStream::Plain(client_stream), Role::Client, None)
            .await;
    let mut server = WebSocketStream::from_raw_socket(server_stream, Role::Server, None).await;
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let server_task = tokio::spawn(async move {
        let Some(Ok(Message::Ping(_))) = server.next().await else {
            return Err(io::Error::other("keepalive ping missing"));
        };
        let _ = release_rx.await;
        Ok(())
    });

    let healthy = probe_idle(client, Duration::from_millis(20)).await;

    assert!(healthy.is_none());
    let _ = release_tx.send(());
    server_task.await.map_err(io::Error::other)??;
    Ok(())
}

#[tokio::test]
async fn active_scope_reclaims_dormant_pool_capacity() -> io::Result<()> {
    let pair_listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let pair_address = pair_listener.local_addr()?;
    let mut idle = Vec::new();
    let mut servers = Vec::new();
    for _ in 0..MAX_POOL_CONNECTIONS {
        let client_stream = TcpStream::connect(pair_address).await?;
        let (server_stream, _) = pair_listener.accept().await?;
        idle.push(
            WebSocketStream::from_raw_socket(
                MaybeTlsStream::Plain(client_stream),
                Role::Client,
                None,
            )
            .await,
        );
        servers.push(WebSocketStream::from_raw_socket(server_stream, Role::Server, None).await);
    }
    let connect_listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let target = Url::parse(&format!(
        "http://{}/v1/responses",
        connect_listener.local_addr()?
    ))
    .map_err(io::Error::other)?;
    let active_scope = HybridScope::new(&target, &HeaderMap::new());
    let metrics = Arc::new(Metrics::default());
    let tls_config = rustls::ClientConfig::builder()
        .with_root_certificates(RootCertStore::empty())
        .with_no_client_auth();
    let pool = HybridPool::new(PrivateTlsConfig::new(Arc::new(tls_config)), metrics);
    {
        let mut state = pool.inner.state.lock().await;
        for (index, upstream) in idle.into_iter().enumerate() {
            let connection_id = u64::try_from(index.saturating_add(1)).map_err(io::Error::other)?;
            let scope = HybridScope {
                target: format!("dormant-{index}"),
                headers: Vec::new(),
            };
            state.scopes.insert(
                scope,
                ScopeState {
                    target: target.clone(),
                    headers: HeaderMap::new(),
                    diagnostics: ScopeDiagnostics::default(),
                    initialized: true,
                    active_local: 0,
                    leased: HashMap::new(),
                    connecting: 0,
                    probing: 0,
                    idle: vec![PoolConnection {
                        id: connection_id,
                        upstream,
                    }],
                },
            );
        }
        state.scopes.insert(
            active_scope.clone(),
            ScopeState {
                target,
                headers: HeaderMap::new(),
                diagnostics: ScopeDiagnostics::default(),
                initialized: false,
                active_local: 1,
                leased: HashMap::new(),
                connecting: 0,
                probing: 0,
                idle: Vec::new(),
            },
        );
    }

    pool.refill(&active_scope).await;

    let state = pool.inner.state.lock().await;
    let active = state
        .scopes
        .get(&active_scope)
        .ok_or_else(|| io::Error::other("active scope missing"))?;
    assert_eq!(active.connecting, 6);
    assert_eq!(
        state.scopes.values().map(total_connections).sum::<usize>(),
        6
    );
    assert_eq!(state.scopes.len(), 1);
    drop(state);
    drop(servers);
    drop(connect_listener);
    Ok(())
}

#[tokio::test]
async fn inactive_scope_keeps_full_warm_reserve() -> io::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let address = listener.local_addr()?;
    let mut clients = Vec::new();
    let mut servers = Vec::new();
    for _ in 0..6 {
        let client_stream = TcpStream::connect(address).await?;
        let (server_stream, _) = listener.accept().await?;
        clients.push(
            WebSocketStream::from_raw_socket(
                MaybeTlsStream::Plain(client_stream),
                Role::Client,
                None,
            )
            .await,
        );
        servers.push(WebSocketStream::from_raw_socket(server_stream, Role::Server, None).await);
    }
    let metrics = Arc::new(Metrics::default());
    let tls_config = rustls::ClientConfig::builder()
        .with_root_certificates(RootCertStore::empty())
        .with_no_client_auth();
    let pool = HybridPool::new(PrivateTlsConfig::new(Arc::new(tls_config)), metrics);
    let target = Url::parse(&format!("http://{address}/v1/responses")).map_err(io::Error::other)?;
    let scope = HybridScope::new(&target, &HeaderMap::new());
    let idle = clients
        .into_iter()
        .enumerate()
        .map(|(index, upstream)| {
            u64::try_from(index.saturating_add(1))
                .map(|id| PoolConnection { id, upstream })
                .map_err(io::Error::other)
        })
        .collect::<io::Result<Vec<_>>>()?;
    let session_id = {
        let mut state = pool.inner.state.lock().await;
        let scope_fingerprint = scope.fingerprint(state.scopes.hasher());
        let session_id = state.register_session(scope_fingerprint);
        state.scopes.insert(
            scope.clone(),
            ScopeState {
                target: target.clone(),
                headers: HeaderMap::new(),
                diagnostics: ScopeDiagnostics::default(),
                initialized: true,
                active_local: 1,
                leased: HashMap::new(),
                connecting: 0,
                probing: 0,
                idle,
            },
        );
        session_id
    };
    pool.unregister(&scope, session_id).await;

    let state = pool.inner.state.lock().await;
    let entry = state
        .scopes
        .get(&scope)
        .ok_or_else(|| io::Error::other("inactive scope missing"))?;
    assert_eq!(entry.idle.len(), 6);
    assert_eq!(
        state.scopes.values().map(total_connections).sum::<usize>(),
        6
    );
    drop(state);
    drop(servers);
    Ok(())
}

#[tokio::test]
async fn successful_checkout_cannot_be_cancelled_after_lease_assignment() -> io::Result<()> {
    // Given: one real idle connection and a second lock waiter queued behind checkout.
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let address = listener.local_addr()?;
    let client_stream = TcpStream::connect(address).await?;
    let (server_stream, _) = listener.accept().await?;
    drop(listener);
    let upstream =
        WebSocketStream::from_raw_socket(MaybeTlsStream::Plain(client_stream), Role::Client, None)
            .await;
    let _server = WebSocketStream::from_raw_socket(server_stream, Role::Server, None).await;
    let target = Url::parse(&format!("http://{address}/v1/responses")).map_err(io::Error::other)?;
    let scope = HybridScope::new(&target, &HeaderMap::new());
    let metrics = Arc::new(Metrics::default());
    let tls_config = rustls::ClientConfig::builder()
        .with_root_certificates(RootCertStore::empty())
        .with_no_client_auth();
    let pool = HybridPool::new(PrivateTlsConfig::new(Arc::new(tls_config)), metrics);
    let session_id = {
        let mut state = pool.inner.state.lock().await;
        let scope_fingerprint = scope.fingerprint(state.scopes.hasher());
        let session_id = state.register_session(scope_fingerprint);
        state.scopes.insert(
            scope.clone(),
            ScopeState {
                target,
                headers: HeaderMap::new(),
                diagnostics: ScopeDiagnostics::default(),
                initialized: true,
                active_local: 1,
                leased: HashMap::new(),
                connecting: 0,
                probing: 0,
                idle: vec![PoolConnection { id: 1, upstream }],
            },
        );
        session_id
    };

    // When: checkout commits the lease while refill's lock acquisition is blocked.
    let state_guard = pool.inner.state.lock().await;
    let checkout_pool = pool.clone();
    let checkout_scope = scope.clone();
    let checkout =
        tokio::spawn(async move { checkout_pool.checkout(&checkout_scope, session_id).await });
    tokio::task::yield_now().await;
    let blocker_pool = pool.clone();
    let (blocked_tx, blocked_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let blocker = tokio::spawn(async move {
        let _state = blocker_pool.inner.state.lock().await;
        let _ = blocked_tx.send(());
        let _ = release_rx.await;
    });
    tokio::task::yield_now().await;
    drop(state_guard);
    tokio::time::timeout(Duration::from_secs(1), blocked_rx)
        .await
        .map_err(io::Error::other)?
        .map_err(io::Error::other)?;
    checkout.abort();
    let _ = release_tx.send(());
    let _ = blocker.await;
    let result = checkout.await;
    pool.unregister(&scope, session_id).await;

    // Then: the successful checkout already returned; abort cannot create a ghost lease.
    assert!(result.is_ok_and(|upstream| upstream.is_some()));
    Ok(())
}
