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

    let metrics = Arc::new(Metrics::default());
    let tls_config = rustls::ClientConfig::builder()
        .with_root_certificates(RootCertStore::empty())
        .with_no_client_auth();
    let pool = HybridPool::new(PrivateTlsConfig::new(Arc::new(tls_config)), metrics);
    let target = Url::parse(&format!("http://{address}/v1/responses")).map_err(io::Error::other)?;
    let headers = HeaderMap::new();
    let scope = HybridScope::new(&target, &headers);
    pool.inner.state.lock().await.scopes.insert(
        scope.clone(),
        ScopeState {
            target,
            headers,
            initialized: false,
            active_local: 0,
            leased: 0,
            connecting: 0,
            probing: 0,
            idle: vec![client],
        },
    );

    pool.maintain_once(&scope, Duration::from_millis(100)).await;

    assert!(pool.checkout(&scope).await.is_some());
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

    let metrics = Arc::new(Metrics::default());
    let tls_config = rustls::ClientConfig::builder()
        .with_root_certificates(RootCertStore::empty())
        .with_no_client_auth();
    let pool = HybridPool::new(PrivateTlsConfig::new(Arc::new(tls_config)), metrics);
    let target = Url::parse(&format!("http://{address}/v1/responses")).map_err(io::Error::other)?;
    let headers = HeaderMap::new();
    let scope = HybridScope::new(&target, &headers);
    pool.inner.state.lock().await.scopes.insert(
        scope.clone(),
        ScopeState {
            target,
            headers,
            initialized: false,
            active_local: 0,
            leased: 0,
            connecting: 0,
            probing: 0,
            idle: vec![client],
        },
    );

    pool.maintain_once(&scope, Duration::from_millis(20)).await;

    assert!(pool.checkout(&scope).await.is_none());
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
            state.scopes.insert(
                HybridScope {
                    target: format!("dormant-{index}"),
                    headers: Vec::new(),
                },
                ScopeState {
                    target: target.clone(),
                    headers: HeaderMap::new(),
                    initialized: true,
                    active_local: 0,
                    leased: 0,
                    connecting: 0,
                    probing: 0,
                    idle: vec![upstream],
                },
            );
        }
        state.scopes.insert(
            active_scope.clone(),
            ScopeState {
                target,
                headers: HeaderMap::new(),
                initialized: false,
                active_local: 1,
                leased: 0,
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
    assert_eq!(active.connecting, 2);
    assert_eq!(
        state.scopes.values().map(total_connections).sum::<usize>(),
        MAX_POOL_CONNECTIONS
    );
    assert_eq!(state.scopes.len(), MAX_POOL_CONNECTIONS - 1);
    drop(state);
    drop(servers);
    drop(connect_listener);
    Ok(())
}

#[tokio::test]
async fn starved_active_scope_reclaims_only_spare_active_capacity() -> io::Result<()> {
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
    let starved_scope = HybridScope::new(&target, &HeaderMap::new());
    let metrics = Arc::new(Metrics::default());
    let tls_config = rustls::ClientConfig::builder()
        .with_root_certificates(RootCertStore::empty())
        .with_no_client_auth();
    let pool = HybridPool::new(PrivateTlsConfig::new(Arc::new(tls_config)), metrics);
    let mut existing_scopes = Vec::new();
    {
        let mut state = pool.inner.state.lock().await;
        for index in 0..4 {
            let scope = HybridScope {
                target: format!("active-{index}"),
                headers: Vec::new(),
            };
            state.scopes.insert(
                scope.clone(),
                ScopeState {
                    target: target.clone(),
                    headers: HeaderMap::new(),
                    initialized: true,
                    active_local: 1,
                    leased: 0,
                    connecting: 0,
                    probing: 0,
                    idle: idle.drain(..2).collect(),
                },
            );
            existing_scopes.push(scope);
        }
        state.scopes.insert(
            starved_scope.clone(),
            ScopeState {
                target,
                headers: HeaderMap::new(),
                initialized: false,
                active_local: 1,
                leased: 0,
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
    assert!(existing_scopes.iter().all(|scope| {
        state
            .scopes
            .get(scope)
            .is_some_and(|entry| total_connections(entry) >= 1)
    }));
    assert_eq!(
        state.scopes.values().map(total_connections).sum::<usize>(),
        MAX_POOL_CONNECTIONS
    );
    drop(state);
    drop(servers);
    drop(connect_listener);
    Ok(())
}
