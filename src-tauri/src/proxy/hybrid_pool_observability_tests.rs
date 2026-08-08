use std::sync::Arc;

use rustls::ClientConfig;

use super::observability::SessionReclaimPolicy;
use super::{ConnectionActivity, ConnectionObservation, HybridPool, PrivateTlsConfig};
use crate::proxy::Metrics;

fn test_pool() -> HybridPool {
    let tls = ClientConfig::builder()
        .with_root_certificates(rustls::RootCertStore::empty())
        .with_no_client_auth();
    HybridPool::new(
        PrivateTlsConfig::new(Arc::new(tls)),
        Arc::new(Metrics::default()),
    )
}

#[tokio::test]
async fn snapshot_exposes_bound_activity_recovery_and_real_reclaim_policy() {
    let pool = test_pool();
    let session_id = pool.register_observed_session().await;

    pool.observe_session(
        session_id,
        ConnectionObservation::Bound {
            thread_id: "thread-9f31a2".to_owned(),
            has_connection: true,
        },
    )
    .await;
    pool.observe_session(
        session_id,
        ConnectionObservation::Active(ConnectionActivity::Up),
    )
    .await;

    let active = pool.connection_snapshot().await;
    assert_eq!(active.bound_threads.len(), 1);
    let Some(bound) = active.bound_threads.first() else {
        panic!("bound connection missing after length assertion");
    };
    assert_eq!(bound.thread_id, "thread-9f31a2");
    assert_eq!(bound.activity, ConnectionActivity::Up);
    assert_eq!(bound.reclaim_policy, SessionReclaimPolicy::ThreadEnd);

    pool.observe_session(
        session_id,
        ConnectionObservation::Recovering {
            reason: "上游连接关闭".to_owned(),
        },
    )
    .await;

    let recovering = pool.connection_snapshot().await;
    assert_eq!(recovering.transitions.len(), 1);
    let Some(transition) = recovering.transitions.first() else {
        panic!("transition missing after length assertion");
    };
    assert_eq!(transition.label, "恢复绑定连接");
    assert_eq!(recovering.recent_closed.len(), 1);
    let Some(closed_connection) = recovering.recent_closed.first() else {
        panic!("closed connection missing after length assertion");
    };
    assert_eq!(closed_connection.reason, "上游连接关闭");

    pool.observe_session(
        session_id,
        ConnectionObservation::Closed {
            reason: "Codex 线程结束".to_owned(),
            normal: true,
        },
    )
    .await;

    let closed = pool.connection_snapshot().await;
    assert!(closed.bound_threads.is_empty());
    assert!(
        !closed
            .recent_closed
            .iter()
            .any(|item| item.reason == "Codex 线程结束")
    );

    let normal_session_id = pool.register_observed_session().await;
    pool.observe_session(
        normal_session_id,
        ConnectionObservation::Bound {
            thread_id: "thread-normal".to_owned(),
            has_connection: true,
        },
    )
    .await;
    pool.observe_session(
        normal_session_id,
        ConnectionObservation::Closed {
            reason: "Codex 线程结束".to_owned(),
            normal: true,
        },
    )
    .await;

    let normally_closed = pool.connection_snapshot().await;
    assert!(
        normally_closed
            .recent_closed
            .iter()
            .any(|item| item.reason == "Codex 线程结束" && item.normal)
    );
}
