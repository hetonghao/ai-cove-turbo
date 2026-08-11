use std::sync::Arc;

use axum::http::HeaderMap;
use rustls::{ClientConfig, RootCertStore};
use url::Url;

use super::{ConnectionObservation, HybridPool, HybridScope, PrivateTlsConfig};
use crate::proxy::Metrics;

#[tokio::test]
async fn snapshot_does_not_project_caller_claim_without_a_pool_lease() -> Result<(), url::ParseError>
{
    // Given: an empty pool with an observation session but no physical resource.
    let tls = ClientConfig::builder()
        .with_root_certificates(RootCertStore::empty())
        .with_no_client_auth();
    let pool = HybridPool::new(
        PrivateTlsConfig::new(Arc::new(tls)),
        Arc::new(Metrics::default()),
    );
    let target = Url::parse("ftp://pool-without-resources.invalid/v1/responses")?;
    let headers = HeaderMap::new();
    let scope = HybridScope::new(&target, &headers);
    let session_id = pool.register(&scope, target, headers).await;

    // When: the caller claims that the session owns a connection.
    pool.observe_session(
        session_id,
        ConnectionObservation::Bound {
            thread_id: "thread-without-lease".to_owned(),
        },
    )
    .await;

    // Then: the snapshot remains grounded in the empty pool resource state.
    let snapshot = pool.connection_snapshot().await;
    assert_eq!(snapshot.current_connections, 0);
    assert_eq!(snapshot.prewarm, 0);
    assert!(snapshot.bound_threads.is_empty());
    Ok(())
}
