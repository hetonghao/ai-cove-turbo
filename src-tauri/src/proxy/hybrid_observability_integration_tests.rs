use std::time::Duration;

use crate::proxy::{ConnectionSnapshot, ProxyHandle, hybrid_pool::ConnectionActivity};

use super::*;

pub(super) const OBSERVED_THREAD_ID: &str = "observed-thread";

pub(super) async fn send_observed_create(client: &mut ClientWebSocket) -> io::Result<()> {
    let turn_metadata = serde_json::json!({
        "session_id": "observability-session",
        "thread_id": OBSERVED_THREAD_ID,
    });
    let request = serde_json::json!({
        "type": "response.create",
        "model": "test",
        "input": "test",
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

fn assert_fresh_connection_observation(active_bound: &Value) -> io::Result<()> {
    let connection_age = active_bound
        .get("connectionAgeSeconds")
        .and_then(Value::as_u64)
        .ok_or_else(|| io::Error::other("connection age missing"))?;
    let probe_age = active_bound
        .get("lastProbeAgeSeconds")
        .and_then(Value::as_u64)
        .ok_or_else(|| io::Error::other("last probe age missing"))?;
    assert!(connection_age < 5);
    assert!(probe_age < 5);
    Ok(())
}

#[tokio::test]
async fn real_hybrid_session_reports_active_idle_and_closed_lifecycle() -> io::Result<()> {
    // Given: 一个已预热的真实 Hybrid WebSocket 会话。
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::HoldResponse,
        delay_http: false,
    })
    .await?;
    let (proxy, metrics) = start_test_proxy(&server).await?;
    let (mut client, status) = connect_local(&proxy).await?;
    assert_eq!(status, 101);
    server.fixture.wait_ready(6).await?;

    // When: Codex 以规范线程 ID 发起请求，上游暂缓响应。
    send_observed_create(&mut client).await?;
    server.fixture.wait_messages(1).await?;
    server.fixture.wait_ready(7).await?;
    assert_eq!(metrics.snapshot().hybrid_ws, 0);

    // Then: 产品快照显示该线程正在上行传输。
    let active = wait_for_snapshot(&proxy, "active upload", |snapshot| {
        snapshot.bound_threads.iter().any(|item| {
            item.thread_id == OBSERVED_THREAD_ID && item.activity == ConnectionActivity::Up
        })
    })
    .await?;
    assert_eq!(active.bound_threads.len(), 1);
    let active_json = serde_json::to_value(&active).map_err(io::Error::other)?;
    let active_bound = active_json
        .get("boundThreads")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .ok_or_else(|| io::Error::other("active bound connection missing"))?;
    assert!(
        active_bound
            .get("upstreamTrace")
            .and_then(Value::as_str)
            .is_some_and(|trace| {
                trace.len() == 32
                    && trace
                        .bytes()
                        .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
            })
    );
    let generation = active_bound
        .get("upstreamGeneration")
        .and_then(Value::as_u64)
        .ok_or_else(|| io::Error::other("upstream generation missing"))?;
    assert!(generation > 0);
    assert_fresh_connection_observation(active_bound)?;
    let expected_id = format!("S{generation:03}");
    assert_eq!(
        active_bound.get("id").and_then(Value::as_str),
        Some(expected_id.as_str())
    );
    assert_eq!(active_bound.get("upstreamOrdinal"), Some(&Value::from(1)));

    // When: 上游完成响应。
    server.fixture.release_private();
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    assert_eq!(metrics.snapshot().hybrid_ws, 1);
    let outcomes = serde_json::to_value(metrics.traffic_snapshot().recent_requests)
        .map_err(io::Error::other)?;
    assert!(outcomes.as_array().is_some_and(|outcomes| {
        outcomes.len() == 1
            && outcomes
                .first()
                .is_some_and(|outcome| outcome.get("result") == Some(&Value::from("success")))
    }));

    // Then: 同一线程保留为空闲绑定连接。
    let idle = wait_for_snapshot(&proxy, "idle binding", |snapshot| {
        snapshot.bound_threads.iter().any(|item| {
            item.thread_id == OBSERVED_THREAD_ID && item.activity == ConnectionActivity::Idle
        })
    })
    .await?;
    assert_eq!(idle.bound_threads.len(), 1);

    let trace = active_bound
        .get("upstreamTrace")
        .and_then(Value::as_str)
        .ok_or_else(|| io::Error::other("upstream trace missing"))?;
    send_observed_create(&mut client).await?;
    server.fixture.wait_messages(2).await?;
    let second_active = wait_for_snapshot(&proxy, "second active upload", |snapshot| {
        snapshot.bound_threads.iter().any(|item| {
            item.thread_id == OBSERVED_THREAD_ID
                && item.activity == ConnectionActivity::Up
                && item.upstream_trace.as_deref() == Some(trace)
                && item.upstream_generation == generation
                && item.upstream_ordinal == 2
        })
    })
    .await?;
    assert_eq!(second_active.bound_threads.len(), 1);
    server.fixture.release_private();
    assert_eq!(next_event_type(&mut client).await?, "response.completed");

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
async fn http_completion_does_not_report_websocket_receive_activity() -> io::Result<()> {
    // Given: 私有连接尚未就绪，首轮请求只能走 HTTP。
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::Delay,
        delay_http: true,
    })
    .await?;
    let (proxy, _) = start_test_proxy(&server).await?;
    let (mut client, status) = connect_local(&proxy).await?;
    assert_eq!(status, 101);

    // When: HTTP 响应完整返回，但私有 WebSocket 仍未建立。
    send_observed_create(&mut client).await?;
    server.fixture.wait_private(1).await?;
    server.fixture.wait_http(1).await?;
    server.fixture.release_http();
    assert_eq!(next_event_type(&mut client).await?, "response.completed");

    // Then: 快照不得把 HTTP 下行伪报为线程专属 WebSocket 正在接收。
    let snapshot = proxy.connection_snapshot().await;
    assert!(
        snapshot
            .bound_threads
            .iter()
            .all(|item| item.thread_id != OBSERVED_THREAD_ID)
    );

    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn delayed_prewarm_binds_idle_session_before_next_request() -> io::Result<()> {
    // Given: 首轮 HTTP 已完成，线程仍保持本地 WebSocket，但池连接尚未就绪。
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::Delay,
        delay_http: false,
    })
    .await?;
    let (proxy, _) = start_test_proxy(&server).await?;
    let (mut client, status) = connect_local(&proxy).await?;
    assert_eq!(status, 101);
    send_observed_create(&mut client).await?;
    server.fixture.wait_private(1).await?;
    server.fixture.wait_http(1).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");

    // When: 池中第一条私有连接随后建立，客户端没有发送第二个请求。
    server.fixture.release_private();
    server.fixture.wait_ready(1).await?;

    // Then: 空闲 Session 自动独占该连接，并保持请求不重放。
    let rebound = wait_for_snapshot(&proxy, "idle binding without another request", |snapshot| {
        snapshot.bound_threads.iter().any(|item| {
            item.thread_id == OBSERVED_THREAD_ID && item.activity == ConnectionActivity::Idle
        })
    })
    .await?;
    assert_eq!(rebound.bound_threads.len(), 1);
    let counts = server.fixture.counts().await;
    assert_eq!(counts.private_messages, 0);
    assert_eq!(counts.http_requests, 1);

    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn snapshot_keeps_six_blank_connections_while_binding_waits() -> io::Result<()> {
    // Given: 一个线程正走 HTTP，六条空白预热连接已建立但尚不能交给活跃请求。
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::Delay,
        delay_http: true,
    })
    .await?;
    let (proxy, _) = start_test_proxy(&server).await?;
    let (mut client, status) = connect_local(&proxy).await?;
    assert_eq!(status, 101);
    send_observed_create(&mut client).await?;
    server.fixture.wait_private(6).await?;
    server.fixture.wait_http(1).await?;
    for _ in 0..6 {
        server.fixture.release_private();
    }
    server.fixture.wait_ready(6).await?;

    // When: 连接检查器读取池状态已经收敛的当前快照。
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(3)).await;
    let snapshot = proxy.connection_snapshot().await;
    tokio::time::resume();

    // Then: 尚无 checkout lease，池中只保留六条真实空白预热。
    assert_eq!(snapshot.current_connections, 6);
    assert_eq!(snapshot.prewarm, 6);
    assert!(snapshot.bound_threads.is_empty());
    let waiting = snapshot
        .transitions
        .iter()
        .find(|item| item.id.starts_with("POOL-BIND-G"))
        .ok_or_else(|| io::Error::other("scope-specific POOL-BIND transition missing"))?;
    assert_eq!(waiting.elapsed_seconds, 3);
    assert!(waiting.detail.contains("连接组 G"));
    assert!(waiting.detail.contains("空白预热 6"));
    assert!(waiting.detail.contains("建立中 0"));

    server.fixture.release_http();
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    server.fixture.wait_private(7).await?;
    server.fixture.release_private();
    server.fixture.wait_ready(7).await?;

    // Then: HTTP 完成后，等待绑定自动收敛为一条空闲绑定与六条真实预热。
    let rebound = wait_for_snapshot(&proxy, "HTTP completion binding convergence", |snapshot| {
        snapshot.current_connections == 7
            && snapshot.prewarm == 6
            && snapshot.bound_threads.iter().any(|item| {
                item.thread_id == OBSERVED_THREAD_ID && item.activity == ConnectionActivity::Idle
            })
    })
    .await?;
    assert_eq!(rebound.current_connections, 7);
    assert_eq!(rebound.prewarm, 6);
    assert_eq!(rebound.bound_threads.len(), 1);

    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn failed_prewarm_reports_sanitized_handshake_reason() -> io::Result<()> {
    // Given: 一个独立连接组的首批私有 WebSocket 握手收到 HTTP 503，随后恢复。
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::FailFirstBatch,
        delay_http: false,
    })
    .await?;
    let (proxy, _) = start_test_proxy(&server).await?;
    let (client, status) =
        connect_local_with_authorization(&proxy, Some("Bearer diagnostic-secret")).await?;
    assert_eq!(status, 101);
    server.fixture.wait_private(12).await?;
    server.fixture.wait_ready(6).await?;

    // When: 连接检查器读取仍在等待的连接组。
    let snapshot = wait_for_snapshot(&proxy, "classified handshake failure", |snapshot| {
        snapshot.transitions.iter().any(|item| {
            item.id.starts_with("POOL-BIND-G") && item.detail.contains("最近握手失败：HTTP 503")
        })
    })
    .await?;

    // Then: 详情足以定位失败类别，但不包含连接身份凭据。
    let waiting = snapshot
        .transitions
        .iter()
        .find(|item| item.id.starts_with("POOL-BIND-G"))
        .ok_or_else(|| io::Error::other("scope-specific POOL-BIND transition missing"))?;
    assert!(waiting.detail.contains("连接组 G"));
    assert!(waiting.detail.contains("最近握手失败：HTTP 503"));
    assert!(waiting.detail.contains("累计 6 次"));
    assert!(!waiting.detail.contains("diagnostic-secret"));

    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn real_hybrid_idle_restart_rebinds_and_records_closed_connection() -> io::Result<()> {
    // Given: 上游会在首个响应完成后以 1012 重启空闲连接。
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::IdleRestart,
        delay_http: false,
    })
    .await?;
    let (proxy, _) = start_test_proxy(&server).await?;
    let (mut client, status) = connect_local(&proxy).await?;
    assert_eq!(status, 101);
    server.fixture.wait_ready(6).await?;

    // When: 绑定线程完成一次真实 WebSocket 请求，随后收到上游重启帧。
    send_observed_create(&mut client).await?;
    server.fixture.wait_messages(1).await?;
    server.fixture.wait_ready(7).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    server.fixture.wait_close_frames(1).await?;

    // Then: 产品快照保留异常关闭原因，并把同一 Session 自动恢复为空闲绑定。
    let rebound = wait_for_snapshot(&proxy, "idle restart rebound", |snapshot| {
        snapshot.bound_threads.iter().any(|item| {
            item.thread_id == OBSERVED_THREAD_ID && item.activity == ConnectionActivity::Idle
        }) && snapshot.recent_closed.iter().any(|item| {
            item.thread_id.as_deref() == Some(OBSERVED_THREAD_ID)
                && item.reason.contains("上游关闭 · 1012")
                && !item.normal
        })
    })
    .await?;
    assert_eq!(rebound.bound_threads.len(), 1);
    let bound_id = rebound
        .bound_threads
        .first()
        .ok_or_else(|| io::Error::other("rebound connection missing"))?
        .id
        .as_str();
    assert!(rebound.transitions.iter().all(|item| item.id != bound_id));

    client.close(None).await.map_err(io::Error::other)?;
    proxy.stop().await;
    server.stop().await;
    Ok(())
}
