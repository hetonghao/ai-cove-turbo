use std::{io, sync::Arc};

use rustls::RootCertStore;
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite::protocol::Role};

use super::*;

#[test]
fn connection_capacity_tracks_demand_without_spares() {
    assert_eq!(desired_connections(0), 0);
    assert_eq!(desired_connections(1), 1);
    assert_eq!(desired_connections(99), 99);
    assert_eq!(desired_connections(100), 100);
    assert_eq!(desired_connections(101), 100);
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
            ScopeBackend {
                target,
                headers: HeaderMap::new(),
                diagnostics: ScopeDiagnostics::default(),
                initialized: true,
                active_local: 1,
                waiting: HashSet::new(),
                leased: HashMap::from([(
                    session_id,
                    ConnectionLease {
                        connection_id: 1,
                        server_trace: None,
                        ordinal: 0,
                        metadata: ConnectionMetadata::fresh(),
                    },
                )]),
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
