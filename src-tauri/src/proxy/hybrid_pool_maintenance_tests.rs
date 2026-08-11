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
                    PoolConnection {
                        id: 1,
                        upstream: first_client,
                    },
                    PoolConnection {
                        id: 2,
                        upstream: second_client,
                    },
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
