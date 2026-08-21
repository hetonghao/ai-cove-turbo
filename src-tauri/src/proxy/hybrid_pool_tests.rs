use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

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
                ScopeBackend {
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
                        server_trace: None,
                        ordinal: 0,
                        metadata: ConnectionMetadata::fresh(),
                    }],
                },
            );
        }
        state.scopes.insert(
            active_scope.clone(),
            ScopeBackend {
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
                .map(|id| PoolConnection {
                    id,
                    upstream,
                    server_trace: None,
                    ordinal: 0,
                    metadata: ConnectionMetadata::fresh(),
                })
                .map_err(io::Error::other)
        })
        .collect::<io::Result<Vec<_>>>()?;
    let session_id = {
        let mut state = pool.inner.state.lock().await;
        let scope_fingerprint = scope.fingerprint(state.scopes.hasher());
        let session_id = state.register_session(scope_fingerprint);
        state.scopes.insert(
            scope.clone(),
            ScopeBackend {
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
            ScopeBackend {
                target,
                headers: HeaderMap::new(),
                diagnostics: ScopeDiagnostics::default(),
                initialized: true,
                active_local: 1,
                leased: HashMap::new(),
                connecting: 0,
                probing: 0,
                idle: vec![PoolConnection {
                    id: 1,
                    upstream,
                    server_trace: None,
                    ordinal: 0,
                    metadata: ConnectionMetadata::fresh(),
                }],
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

#[tokio::test]
async fn session_handle_allows_one_typed_lease_and_close_is_terminal() -> io::Result<()> {
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
    let tls_config = rustls::ClientConfig::builder()
        .with_root_certificates(RootCertStore::empty())
        .with_no_client_auth();
    let pool = HybridPool::new(
        PrivateTlsConfig::new(Arc::new(tls_config)),
        Arc::new(crate::proxy::Metrics::default()),
    );
    let session = {
        let mut state = pool.inner.state.lock().await;
        let fingerprint = scope.fingerprint(state.scopes.hasher());
        let session_id = state.register_session(fingerprint);
        state.scopes.insert(
            scope.clone(),
            ScopeBackend {
                target: target.clone(),
                headers: HeaderMap::new(),
                diagnostics: ScopeDiagnostics::default(),
                initialized: true,
                active_local: 1,
                leased: HashMap::new(),
                connecting: 0,
                probing: 0,
                idle: vec![PoolConnection {
                    id: 1,
                    upstream,
                    server_trace: None,
                    ordinal: 0,
                    metadata: ConnectionMetadata::fresh(),
                }],
            },
        );
        drop(state);
        SessionHandle::new(pool.clone(), scope.clone(), session_id)
    };
    let session = session;

    let mut lease = session
        .checkout()
        .await
        .ok_or_else(|| io::Error::other("typed lease missing"))?;
    assert!(session.checkout().await.is_none());
    session.close().await;
    assert!(session.checkout().await.is_none());
    lease.release().await;
    lease.release().await;
    Ok(())
}

#[tokio::test]
async fn open_session_handle_close_is_idempotent() -> io::Result<()> {
    let tls_config = rustls::ClientConfig::builder()
        .with_root_certificates(RootCertStore::empty())
        .with_no_client_auth();
    let pool = HybridPool::new(
        PrivateTlsConfig::new(Arc::new(tls_config)),
        Arc::new(crate::proxy::Metrics::default()),
    );
    let target = Url::parse("http://127.0.0.1:9/v1/responses").map_err(io::Error::other)?;
    let scope = HybridScope::new(&target, &HeaderMap::new());
    let session = pool.open_session(&scope, target, HeaderMap::new()).await;

    assert!(!session.is_closed());
    session.close().await;
    session.close().await;
    assert!(session.is_closed());
    assert!(
        tokio::time::timeout(Duration::from_millis(50), session.checkout_ready())
            .await
            .is_ok_and(|lease| lease.is_none())
    );
    Ok(())
}

#[tokio::test]
async fn cancelled_typed_checkout_releases_local_lease_gate() -> io::Result<()> {
    let tls_config = rustls::ClientConfig::builder()
        .with_root_certificates(RootCertStore::empty())
        .with_no_client_auth();
    let pool = HybridPool::new(
        PrivateTlsConfig::new(Arc::new(tls_config)),
        Arc::new(crate::proxy::Metrics::default()),
    );
    let target = Url::parse("http://127.0.0.1:9/v1/responses").map_err(io::Error::other)?;
    let scope = HybridScope::new(&target, &HeaderMap::new());
    let session = SessionHandle::new(pool, scope, 1);

    let pending = tokio::time::timeout(
        Duration::from_millis(10),
        session.checkout_with(std::future::pending),
    )
    .await;

    assert!(pending.is_err());
    assert!(!session.is_lease_active());
    Ok(())
}

#[tokio::test]
async fn typed_lease_terminal_transitions_are_idempotent() -> io::Result<()> {
    let tls_config = rustls::ClientConfig::builder()
        .with_root_certificates(RootCertStore::empty())
        .with_no_client_auth();
    let pool = HybridPool::new(
        PrivateTlsConfig::new(Arc::new(tls_config)),
        Arc::new(crate::proxy::Metrics::default()),
    );
    let target = Url::parse("http://127.0.0.1:9/v1/responses").map_err(io::Error::other)?;
    let scope = HybridScope::new(&target, &HeaderMap::new());

    let owner_active = Arc::new(AtomicBool::new(true));
    let mut discarded =
        Lease::active_without_upstream(pool.clone(), scope.clone(), 1, Arc::clone(&owner_active));
    discarded.discard(LeaseRetirement::Replacing).await;
    discarded.release().await;
    assert_eq!(discarded.state(), LeaseState::Discarded);
    assert!(!owner_active.load(Ordering::Acquire));

    let owner_active = Arc::new(AtomicBool::new(true));
    let mut park_without_connection =
        Lease::active_without_upstream(pool, scope, 2, Arc::clone(&owner_active));
    assert!(
        park_without_connection
            .park("thread-1".to_owned(), "response-1".to_owned())
            .await
            .is_err()
    );
    assert_eq!(park_without_connection.state(), LeaseState::Active);
    assert!(owner_active.load(Ordering::Acquire));
    Ok(())
}

#[test]
fn scope_backend_keeps_local_state_under_shared_capacity_cap() -> io::Result<()> {
    let target =
        Url::parse("https://scope-backend.invalid/v1/responses").map_err(io::Error::other)?;
    let backend = || ScopeBackend {
        target: target.clone(),
        headers: HeaderMap::new(),
        diagnostics: ScopeDiagnostics::default(),
        initialized: false,
        active_local: 0,
        leased: HashMap::new(),
        connecting: 0,
        probing: 0,
        idle: Vec::new(),
    };
    let mut first = backend();
    let mut second = backend();

    first.add_active_local();
    first.add_connecting(2);
    second.add_probing();

    assert_eq!(first.active_local, 1);
    assert_eq!(first.connecting, 2);
    assert_eq!(total_connections(&first), 2);
    assert_eq!(second.active_local, 0);
    assert_eq!(second.probing, 1);
    assert_eq!(total_connections(&second), 1);
    assert_eq!(MAX_POOL_CONNECTIONS, 100);
    Ok(())
}
