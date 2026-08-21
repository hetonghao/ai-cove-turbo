use std::{io, sync::Arc, time::Duration};

use futures_util::{SinkExt, StreamExt};
use rustls::RootCertStore;
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{Message, protocol::Role},
};

use super::*;

fn pool_connection(
    id: u64,
    upstream: PrivateUpstream,
    last_probe_at: Option<tokio::time::Instant>,
) -> PoolConnection {
    PoolConnection {
        id,
        upstream,
        server_trace: None,
        ordinal: 0,
        last_probe_at,
    }
}

#[tokio::test]
async fn checkout_remains_available_while_keepalive_probe_waits() -> io::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let address = listener.local_addr()?;
    let first_client_stream = TcpStream::connect(address).await?;
    let (first_server_stream, _) = listener.accept().await?;
    let second_client_stream = TcpStream::connect(address).await?;
    let (second_server_stream, _) = listener.accept().await?;
    let first_client = WebSocketStream::from_raw_socket(
        MaybeTlsStream::Plain(first_client_stream),
        Role::Client,
        None,
    )
    .await;
    let second_client = WebSocketStream::from_raw_socket(
        MaybeTlsStream::Plain(second_client_stream),
        Role::Client,
        None,
    )
    .await;
    let mut first_server =
        WebSocketStream::from_raw_socket(first_server_stream, Role::Server, None).await;
    let mut second_server =
        WebSocketStream::from_raw_socket(second_server_stream, Role::Server, None).await;
    let (ping_seen, ping_wait) = tokio::sync::oneshot::channel();
    let (release, released) = tokio::sync::oneshot::channel();
    let first_server_task = tokio::spawn(async move {
        let Some(Ok(Message::Ping(payload))) = first_server.next().await else {
            return Err(io::Error::other("first keepalive ping missing"));
        };
        let _ = ping_seen.send(());
        let _ = released.await;
        first_server
            .send(Message::Pong(payload))
            .await
            .map_err(io::Error::other)
    });
    let second_server_task = tokio::spawn(async move {
        let Some(Ok(Message::Ping(payload))) = second_server.next().await else {
            return Ok(());
        };
        second_server
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
                active_local: 0,
                leased: HashMap::new(),
                connecting: 4,
                probing: 0,
                idle: vec![
                    pool_connection(1, first_client, Some(tokio::time::Instant::now())),
                    pool_connection(2, second_client, Some(tokio::time::Instant::now())),
                ],
            },
        );
        session_id
    };
    let maintaining = tokio::spawn({
        let pool = pool.clone();
        let scope = scope.clone();
        async move {
            pool.maintain_once(&scope, Duration::from_millis(200)).await;
        }
    });
    ping_wait.await.map_err(io::Error::other)?;

    let available = pool.checkout(&scope, session_id).await.is_some();
    let _ = release.send(());
    maintaining.await.map_err(io::Error::other)?;
    first_server_task.await.map_err(io::Error::other)??;
    second_server_task.await.map_err(io::Error::other)??;

    assert!(available);
    Ok(())
}

#[tokio::test]
async fn sole_idle_reserve_is_probed_before_it_is_reused() -> io::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let address = listener.local_addr()?;
    let client_stream = TcpStream::connect(address).await?;
    let (server_stream, _) = listener.accept().await?;
    let client =
        WebSocketStream::from_raw_socket(MaybeTlsStream::Plain(client_stream), Role::Client, None)
            .await;
    let mut server = WebSocketStream::from_raw_socket(server_stream, Role::Server, None).await;
    let (ping_seen, ping_wait) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let server_task = tokio::spawn(async move {
        let Some(Ok(Message::Ping(payload))) = server.next().await else {
            return Err(io::Error::other("sole reserve keepalive ping missing"));
        };
        let _ = ping_seen.send(());
        let _ = release_rx.await;
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
                leased: HashMap::new(),
                connecting: 0,
                probing: 0,
                idle: vec![pool_connection(1, client, None)],
            },
        );
        session_id
    };

    let maintaining = tokio::spawn({
        let pool = pool.clone();
        let scope = scope.clone();
        async move { pool.maintain_once(&scope, Duration::from_millis(200)).await }
    });
    tokio::time::timeout(Duration::from_millis(200), ping_wait)
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "sole reserve was not probed"))?
        .map_err(io::Error::other)?;
    assert_eq!(
        pool.inner
            .state
            .lock()
            .await
            .scopes
            .get(&scope)
            .map_or(0, |entry| entry.probing),
        1
    );

    let checkout = tokio::spawn({
        let pool = pool.clone();
        let scope = scope.clone();
        async move { pool.checkout(&scope, session_id).await }
    });
    tokio::task::yield_now().await;
    let _ = release_tx.send(());
    maintaining.await.map_err(io::Error::other)?;
    let leased = checkout.await.map_err(io::Error::other)?;
    let state = pool.inner.state.lock().await;
    let entry = state
        .scopes
        .get(&scope)
        .ok_or_else(|| io::Error::other("sole reserve scope disappeared"))?;
    assert!(
        leased.is_some(),
        "checkout returned none: idle={}, leased={}, probing={}, connecting={}",
        entry.idle.len(),
        entry.leased.len(),
        entry.probing,
        entry.connecting
    );
    drop(state);
    server_task.await.map_err(io::Error::other)??;
    Ok(())
}

#[tokio::test]
async fn cancelled_checkout_preflight_releases_probe_slot() -> io::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let address = listener.local_addr()?;
    let client_stream = TcpStream::connect(address).await?;
    let (server_stream, _) = listener.accept().await?;
    let client =
        WebSocketStream::from_raw_socket(MaybeTlsStream::Plain(client_stream), Role::Client, None)
            .await;
    let mut server = WebSocketStream::from_raw_socket(server_stream, Role::Server, None).await;
    let (ping_seen, ping_wait) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let server_task = tokio::spawn(async move {
        let Some(Ok(Message::Ping(_))) = server.next().await else {
            return Err(io::Error::other("cancelled preflight ping missing"));
        };
        let _ = ping_seen.send(());
        let _ = release_rx.await;
        Ok(())
    });
    let tls_config = rustls::ClientConfig::builder()
        .with_root_certificates(RootCertStore::empty())
        .with_no_client_auth();
    let pool = HybridPool::new(
        PrivateTlsConfig::new(Arc::new(tls_config)),
        Arc::new(Metrics::default()),
    );
    let target = Url::parse(&format!("http://{address}/v1/responses")).map_err(io::Error::other)?;
    let scope = HybridScope::new(&target, &HeaderMap::new());
    let session_id = {
        let mut state = pool.inner.state.lock().await;
        let fingerprint = scope.fingerprint(state.scopes.hasher());
        let session_id = state.register_session(fingerprint);
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
                idle: vec![pool_connection(1, client, None)],
            },
        );
        session_id
    };
    let checkout = tokio::spawn({
        let pool = pool.clone();
        let scope = scope.clone();
        async move { pool.checkout(&scope, session_id).await }
    });
    ping_wait.await.map_err(io::Error::other)?;
    checkout.abort();
    let _ = release_tx.send(());
    server_task.await.map_err(io::Error::other)??;

    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let probing = pool
                .inner
                .state
                .lock()
                .await
                .scopes
                .get(&scope)
                .map_or(0, |entry| entry.probing);
            if probing == 0 {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .map_err(|_| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            "cancelled probe slot was not released",
        )
    })?;
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
async fn checkout_replaces_failed_preflight_before_leasing_application_work() -> io::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let address = listener.local_addr()?;
    let stale_client_stream = TcpStream::connect(address).await?;
    let (stale_server_stream, _) = listener.accept().await?;
    let healthy_client_stream = TcpStream::connect(address).await?;
    let (healthy_server_stream, _) = listener.accept().await?;
    drop(listener);
    let stale_client = WebSocketStream::from_raw_socket(
        MaybeTlsStream::Plain(stale_client_stream),
        Role::Client,
        None,
    )
    .await;
    let healthy_client = WebSocketStream::from_raw_socket(
        MaybeTlsStream::Plain(healthy_client_stream),
        Role::Client,
        None,
    )
    .await;
    let mut stale_server =
        WebSocketStream::from_raw_socket(stale_server_stream, Role::Server, None).await;
    let _healthy_server =
        WebSocketStream::from_raw_socket(healthy_server_stream, Role::Server, None).await;
    let stale_server_task = tokio::spawn(async move {
        let Some(Ok(Message::Ping(_))) = stale_server.next().await else {
            return Err(io::Error::other("stale preflight ping missing"));
        };
        drop(stale_server);
        Ok(())
    });
    let metrics = Arc::new(Metrics::default());
    let tls_config = rustls::ClientConfig::builder()
        .with_root_certificates(RootCertStore::empty())
        .with_no_client_auth();
    let pool = HybridPool::new(PrivateTlsConfig::new(Arc::new(tls_config)), metrics);
    let target = Url::parse("http://127.0.0.1:1/v1/responses").map_err(io::Error::other)?;
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
                leased: HashMap::new(),
                connecting: 0,
                probing: 0,
                idle: vec![
                    pool_connection(1, healthy_client, Some(tokio::time::Instant::now())),
                    pool_connection(2, stale_client, None),
                ],
            },
        );
        session_id
    };

    let leased = tokio::time::timeout(Duration::from_secs(1), pool.checkout(&scope, session_id))
        .await
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "checkout preflight did not recover",
            )
        })?;

    assert!(leased.is_some());
    stale_server_task.await.map_err(io::Error::other)??;
    let state = pool.inner.state.lock().await;
    let entry = state
        .scopes
        .get(&scope)
        .ok_or_else(|| io::Error::other("scope disappeared after preflight replacement"))?;
    assert!(entry.leased.contains_key(&session_id));
    assert_eq!(
        entry
            .leased
            .get(&session_id)
            .map(|lease| lease.connection_id),
        Some(1)
    );
    drop(state);
    Ok(())
}
