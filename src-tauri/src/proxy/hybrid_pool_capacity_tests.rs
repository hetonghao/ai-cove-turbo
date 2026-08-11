use std::{io, sync::Arc};

use rustls::RootCertStore;
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite::protocol::Role};

use super::*;

#[test]
fn warm_reserve_decreases_with_active_sessions() {
    assert_eq!(desired_connections(0), 6);
    assert_eq!(desired_connections(4), 10);
    assert_eq!(desired_connections(5), 10);
    assert_eq!(desired_connections(10), 14);
    assert_eq!(desired_connections(15), 18);
    assert_eq!(desired_connections(20), 22);
    assert_eq!(desired_connections(25), 26);
    assert_eq!(desired_connections(30), 31);
}

#[test]
fn warm_spare_yields_at_one_hundred_connection_ceiling() {
    assert_eq!(desired_connections(99), 100);
    assert_eq!(desired_connections(100), 100);
}

#[tokio::test]
async fn recovering_isolated_scope_reuses_global_warm_reserve() -> io::Result<()> {
    // Given: one scope already owns the six process-wide recovery slots.
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let address = listener.local_addr()?;
    let client_stream = TcpStream::connect(address).await?;
    let (server_stream, _) = listener.accept().await?;
    let idle =
        WebSocketStream::from_raw_socket(MaybeTlsStream::Plain(client_stream), Role::Client, None)
            .await;
    let _server = WebSocketStream::from_raw_socket(server_stream, Role::Server, None).await;
    let target = Url::parse(&format!("http://{address}/v1/responses")).map_err(io::Error::other)?;
    let metrics = Arc::new(Metrics::default());
    let tls_config = rustls::ClientConfig::builder()
        .with_root_certificates(RootCertStore::empty())
        .with_no_client_auth();
    let pool = HybridPool::new(PrivateTlsConfig::new(Arc::new(tls_config)), metrics);
    let first_scope = HybridScope {
        target: "first".to_owned(),
        headers: Vec::new(),
    };
    let second_scope = HybridScope {
        target: "second".to_owned(),
        headers: Vec::new(),
    };
    {
        let mut state = pool.inner.state.lock().await;
        state.scopes.insert(
            first_scope.clone(),
            ScopeState {
                target: target.clone(),
                headers: HeaderMap::new(),
                diagnostics: ScopeDiagnostics::default(),
                initialized: true,
                active_local: 1,
                leased: HashMap::new(),
                connecting: 5,
                probing: 0,
                idle: vec![PoolConnection {
                    id: 1,
                    upstream: idle,
                }],
            },
        );
        state.scopes.insert(
            second_scope.clone(),
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

    // When: a second isolated scope joins during recovery.
    pool.refill(&second_scope).await;

    // Then: one ready slot moves scopes without multiplying the global reserve.
    let state = pool.inner.state.lock().await;
    assert_eq!(
        state
            .scopes
            .get(&second_scope)
            .ok_or_else(|| io::Error::other("second scope missing"))?
            .connecting,
        1
    );
    assert!(
        state
            .scopes
            .get(&first_scope)
            .ok_or_else(|| io::Error::other("first scope missing"))?
            .idle
            .is_empty()
    );
    assert_eq!(
        state.scopes.values().map(total_connections).sum::<usize>(),
        6
    );
    drop(state);
    drop(listener);
    Ok(())
}

#[tokio::test]
async fn released_session_connection_is_not_returned_to_blank_pool() -> io::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let address = listener.local_addr()?;
    let client_stream = TcpStream::connect(address).await?;
    let (server_stream, _) = listener.accept().await?;
    let client =
        WebSocketStream::from_raw_socket(MaybeTlsStream::Plain(client_stream), Role::Client, None)
            .await;
    let _server = WebSocketStream::from_raw_socket(server_stream, Role::Server, None).await;
    let metrics = Arc::new(Metrics::default());
    let tls_config = rustls::ClientConfig::builder()
        .with_root_certificates(RootCertStore::empty())
        .with_no_client_auth();
    let pool = HybridPool::new(PrivateTlsConfig::new(Arc::new(tls_config)), metrics);
    let target = Url::parse(&format!("http://{address}/v1/responses")).map_err(io::Error::other)?;
    let scope = HybridScope::new(&target, &HeaderMap::new());
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
                leased: HashMap::from([(session_id, ConnectionLease { connection_id: 1 })]),
                connecting: 0,
                probing: 0,
                idle: Vec::new(),
            },
        );
        session_id
    };

    pool.release_session_connection(&scope, session_id, Some(client))
        .await;

    assert!(pool.checkout(&scope, session_id).await.is_none());
    assert_eq!(
        pool.inner
            .state
            .lock()
            .await
            .scopes
            .get(&scope)
            .map_or(0, |entry| entry.leased.len()),
        0
    );
    Ok(())
}

#[tokio::test]
async fn starved_active_scope_reclaims_one_shared_ready_slot() -> io::Result<()> {
    let pair_listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let pair_address = pair_listener.local_addr()?;
    let mut idle = Vec::new();
    let mut servers = Vec::new();
    for index in 0..MAX_POOL_CONNECTIONS {
        let connection_id = u64::try_from(index.saturating_add(1)).map_err(io::Error::other)?;
        let client_stream = TcpStream::connect(pair_address).await?;
        let (server_stream, _) = pair_listener.accept().await?;
        let upstream = WebSocketStream::from_raw_socket(
            MaybeTlsStream::Plain(client_stream),
            Role::Client,
            None,
        )
        .await;
        idle.push(PoolConnection {
            id: connection_id,
            upstream,
        });
        servers.push(WebSocketStream::from_raw_socket(server_stream, Role::Server, None).await);
    }
    let connect_listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let target = Url::parse(&format!(
        "http://{}/v1/responses",
        connect_listener.local_addr()?
    ))
    .map_err(io::Error::other)?;
    let starved_scope = HybridScope::new(&target, &HeaderMap::new());
    let metrics = Arc::new(Metrics::default());
    let tls_config = rustls::ClientConfig::builder()
        .with_root_certificates(RootCertStore::empty())
        .with_no_client_auth();
    let pool = HybridPool::new(PrivateTlsConfig::new(Arc::new(tls_config)), metrics);
    {
        let mut state = pool.inner.state.lock().await;
        for index in 0..(MAX_POOL_CONNECTIONS / 2) {
            let scope = HybridScope {
                target: format!("active-{index}"),
                headers: Vec::new(),
            };
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
                    idle: idle.drain(..2).collect(),
                },
            );
        }
        state.scopes.insert(
            starved_scope.clone(),
            ScopeState {
                target,
                headers: HeaderMap::new(),
                diagnostics: ScopeDiagnostics::default(),
                initialized: false,
                active_local: 10,
                leased: HashMap::new(),
                connecting: 0,
                probing: 0,
                idle: Vec::new(),
            },
        );
    }

    pool.refill(&starved_scope).await;

    let state = pool.inner.state.lock().await;
    assert_eq!(
        state
            .scopes
            .get(&starved_scope)
            .ok_or_else(|| io::Error::other("starved scope missing"))?
            .connecting,
        1
    );
    assert_eq!(
        state.scopes.values().map(total_connections).sum::<usize>(),
        6
    );
    drop(state);
    drop(servers);
    drop(connect_listener);
    Ok(())
}
