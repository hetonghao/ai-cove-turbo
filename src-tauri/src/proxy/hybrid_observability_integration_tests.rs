use std::time::Duration;

use crate::proxy::{ConnectionSnapshot, ProxyHandle, hybrid_pool::ConnectionActivity};

use super::*;

const OBSERVED_THREAD_ID: &str = "observed-thread";

async fn send_observed_create(client: &mut ClientWebSocket) -> io::Result<()> {
    let turn_metadata = serde_json::json!({
        "session_id": "observability-session",
        "thread_id": OBSERVED_THREAD_ID,
    });
    let request = serde_json::json!({
        "type": "response.create",
        "model": "test",
        "input": [],
        "client_metadata": {
            "session_id": "observability-session",
            "thread_id": OBSERVED_THREAD_ID,
            "x-codex-turn-metadata": turn_metadata.to_string(),
        },
    });
    client
        .send(Message::Text(request.to_string().into()))
        .await
        .map_err(io::Error::other)
}

async fn wait_for_snapshot(
    proxy: &ProxyHandle,
    expected: &'static str,
    predicate: impl Fn(&ConnectionSnapshot) -> bool + Send + Sync,
) -> io::Result<ConnectionSnapshot> {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let snapshot = proxy.connection_snapshot().await;
            if predicate(&snapshot) {
                return snapshot;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .map_err(|_| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            format!("connection snapshot did not report {expected}"),
        )
    })
}

#[tokio::test]
async fn real_hybrid_session_reports_active_idle_and_closed_lifecycle() -> io::Result<()> {
    // Given: 一个已预热的真实 Hybrid WebSocket 会话。
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::HoldResponse,
        delay_http: false,
    })
    .await?;
    let (proxy, _) = start_test_proxy(&server).await?;
    let (mut client, status) = connect_local(&proxy).await?;
    assert_eq!(status, 101);
    server.fixture.wait_ready(7).await?;

    // When: Codex 以规范线程 ID 发起请求，上游暂缓响应。
    send_observed_create(&mut client).await?;
    server.fixture.wait_messages(1).await?;

    // Then: 产品快照显示该线程正在上行传输。
    let active = wait_for_snapshot(&proxy, "active upload", |snapshot| {
        snapshot.bound_threads.iter().any(|item| {
            item.thread_id == OBSERVED_THREAD_ID && item.activity == ConnectionActivity::Up
        })
    })
    .await?;
    assert_eq!(active.bound_threads.len(), 1);

    // When: 上游完成响应。
    server.fixture.release_private();
    assert_eq!(next_event_type(&mut client).await?, "response.completed");

    // Then: 同一线程保留为空闲绑定连接。
    let idle = wait_for_snapshot(&proxy, "idle binding", |snapshot| {
        snapshot.bound_threads.iter().any(|item| {
            item.thread_id == OBSERVED_THREAD_ID && item.activity == ConnectionActivity::Idle
        })
    })
    .await?;
    assert_eq!(idle.bound_threads.len(), 1);

    // When: Codex 关闭本地会话。
    client.close(None).await.map_err(io::Error::other)?;

    // Then: 绑定连接从常驻列表移除，并记录为线程结束后的正常关闭。
    let closed = wait_for_snapshot(&proxy, "normal thread close", |snapshot| {
        snapshot.bound_threads.is_empty()
            && snapshot.recent_closed.iter().any(|item| {
                item.thread_id.as_deref() == Some(OBSERVED_THREAD_ID)
                    && item.reason == "Codex 线程结束"
                    && item.normal
            })
    })
    .await?;
    assert_eq!(closed.recent_closed.len(), 1);

    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn real_hybrid_idle_restart_reports_recovery_transition() -> io::Result<()> {
    // Given: 上游会在首个响应完成后以 1012 重启空闲连接。
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::IdleRestart,
        delay_http: false,
    })
    .await?;
    let (proxy, _) = start_test_proxy(&server).await?;
    let (mut client, status) = connect_local(&proxy).await?;
    assert_eq!(status, 101);
    server.fixture.wait_ready(7).await?;

    // When: 绑定线程完成一次真实 WebSocket 请求，随后收到上游重启帧。
    send_observed_create(&mut client).await?;
    server.fixture.wait_messages(1).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    server.fixture.wait_restarts(1).await?;

    // Then: 产品快照显示该线程正在恢复，并保留异常关闭原因。
    let recovering = wait_for_snapshot(&proxy, "idle restart recovery", |snapshot| {
        snapshot
            .transitions
            .iter()
            .any(|item| item.label == "恢复绑定连接")
            && snapshot.recent_closed.iter().any(|item| {
                item.thread_id.as_deref() == Some(OBSERVED_THREAD_ID)
                    && item.reason.contains("上游关闭 · 1012")
                    && !item.normal
            })
    })
    .await?;
    assert!(recovering.bound_threads.is_empty());

    client.close(None).await.map_err(io::Error::other)?;
    proxy.stop().await;
    server.stop().await;
    Ok(())
}
