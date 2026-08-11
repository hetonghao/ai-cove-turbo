use super::*;

async fn send_thread_create(client: &mut ClientWebSocket, thread_id: &str) -> io::Result<()> {
    let turn_metadata = serde_json::json!({
        "session_id": "shared-root-session",
        "thread_id": thread_id,
    });
    let request = serde_json::json!({
        "type": "response.create",
        "model": "test",
        "input": [],
        "client_metadata": {
            "session_id": "shared-root-session",
            "thread_id": thread_id,
            "x-codex-turn-metadata": turn_metadata.to_string(),
        },
    });
    client
        .send(Message::Text(request.to_string().into()))
        .await
        .map_err(io::Error::other)
}

async fn send_thread_continuation(
    client: &mut ClientWebSocket,
    thread_id: &str,
    previous_response_id: &str,
) -> io::Result<()> {
    let turn_metadata = serde_json::json!({
        "session_id": "shared-root-session",
        "thread_id": thread_id,
    });
    let request = serde_json::json!({
        "type": "response.create",
        "model": "test",
        "input": [],
        "previous_response_id": previous_response_id,
        "client_metadata": {
            "session_id": "shared-root-session",
            "thread_id": thread_id,
            "x-codex-turn-metadata": turn_metadata.to_string(),
        },
    });
    client
        .send(Message::Text(request.to_string().into()))
        .await
        .map_err(io::Error::other)
}

async fn expect_protocol_close(client: &mut ClientWebSocket) -> io::Result<()> {
    let close = tokio::time::timeout(std::time::Duration::from_secs(1), client.next())
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "protocol close missing"))?;
    let Some(Ok(Message::Close(Some(frame)))) = close else {
        return Err(io::Error::other("protocol close frame missing"));
    };
    assert_eq!(u16::from(frame.code), 1002);
    assert_eq!(frame.reason, "同一 WebSocket 不能切换 Codex 会话");
    Ok(())
}

#[tokio::test]
async fn local_websocket_rejects_switching_codex_threads() -> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::Persistent,
        delay_http: false,
    })
    .await?;
    let (proxy, _) = start_test_proxy(&server).await?;
    let (mut client, status) = connect_local(&proxy).await?;
    assert_eq!(status, 101);
    server.fixture.wait_ready(6).await?;

    send_thread_create(&mut client, "parent-thread").await?;
    server.fixture.wait_messages(1).await?;
    server.fixture.wait_ready(7).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");

    send_thread_create(&mut client, "cloned-thread").await?;
    assert_eq!(next_event_type(&mut client).await?, "error");
    expect_protocol_close(&mut client).await?;
    assert_counts(server.fixture.counts().await, 7, 1, 0);

    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn canonical_first_request_overrides_stale_handshake_thread() -> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::Persistent,
        delay_http: false,
    })
    .await?;
    let (proxy, _) = start_test_proxy(&server).await?;
    let (mut client, status) =
        connect_local_with_headers(&proxy, None, Some("stale-parent-thread")).await?;
    assert_eq!(status, 101);
    server.fixture.wait_ready(6).await?;

    send_thread_create(&mut client, "cloned-thread").await?;
    server.fixture.wait_messages(1).await?;
    server.fixture.wait_ready(7).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    assert_counts(server.fixture.counts().await, 7, 1, 0);

    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn active_response_rejects_switching_codex_threads() -> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::Delay,
        delay_http: true,
    })
    .await?;
    let (proxy, _) = start_test_proxy(&server).await?;
    let (mut client, status) = connect_local(&proxy).await?;
    assert_eq!(status, 101);
    server.fixture.wait_private(6).await?;

    send_thread_create(&mut client, "parent-thread").await?;
    server.fixture.wait_http(1).await?;
    send_thread_create(&mut client, "cloned-thread").await?;
    assert_eq!(next_event_type(&mut client).await?, "error");
    expect_protocol_close(&mut client).await?;
    assert_counts(server.fixture.counts().await, 6, 0, 1);

    server.fixture.release_http();
    for _ in 0..6 {
        server.fixture.release_private();
    }
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn local_websocket_keeps_same_codex_thread_across_turns() -> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::Persistent,
        delay_http: false,
    })
    .await?;
    let (proxy, _) = start_test_proxy(&server).await?;
    let (mut client, status) = connect_local(&proxy).await?;
    assert_eq!(status, 101);
    server.fixture.wait_ready(6).await?;

    send_thread_create(&mut client, "parent-thread").await?;
    server.fixture.wait_messages(1).await?;
    server.fixture.wait_ready(7).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    send_thread_create(&mut client, "parent-thread").await?;
    server.fixture.wait_messages(2).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    assert_counts(server.fixture.counts().await, 7, 2, 0);

    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn cancelled_response_allows_next_serial_create() -> io::Result<()> {
    // Given: the first HTTP response is active on one local Hybrid WebSocket.
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::Delay,
        delay_http: true,
    })
    .await?;
    let (proxy, _) = start_test_proxy(&server).await?;
    let (mut client, status) = connect_local(&proxy).await?;
    assert_eq!(status, 101);
    send_create(&mut client).await?;
    server.fixture.wait_private(1).await?;
    server.fixture.wait_http(1).await?;

    // When: cancellation reaches its terminal event, then the same socket starts another turn.
    send_cancel(&mut client).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.cancelled");
    server.fixture.release_http();
    send_create(&mut client).await?;
    server.fixture.wait_http(2).await?;
    server.fixture.wait_private(6).await?;
    server.fixture.release_http();

    // Then: the next serial request completes once without replaying either request.
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    assert_counts(server.fixture.counts().await, 6, 0, 2);

    drop(client);
    for _ in 0..6 {
        server.fixture.release_private();
    }
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn same_codex_thread_can_use_two_isolated_websockets_concurrently() -> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::Persistent,
        delay_http: false,
    })
    .await?;
    let (proxy, _) = start_test_proxy(&server).await?;
    let (mut first, first_status) = connect_local(&proxy).await?;
    let (mut second, second_status) = connect_local(&proxy).await?;
    assert_eq!(first_status, 101);
    assert_eq!(second_status, 101);
    server.fixture.wait_ready(6).await?;

    let (first_send, second_send) = tokio::join!(
        send_thread_create(&mut first, "shared-thread"),
        send_thread_create(&mut second, "shared-thread"),
    );
    first_send?;
    second_send?;
    server.fixture.wait_messages(2).await?;
    server.fixture.wait_ready(8).await?;
    assert_eq!(next_event_type(&mut first).await?, "response.completed");
    assert_eq!(next_event_type(&mut second).await?, "response.completed");
    assert_counts(server.fixture.counts().await, 8, 2, 0);

    drop(first);
    drop(second);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn same_thread_reconnect_reclaims_its_stateful_websocket() -> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::Stateful,
        delay_http: false,
    })
    .await?;
    let (proxy, _) = start_test_proxy(&server).await?;
    let (mut first, first_status) = connect_local(&proxy).await?;
    assert_eq!(first_status, 101);
    server.fixture.wait_ready(6).await?;

    send_thread_create(&mut first, "reconnecting-thread").await?;
    server.fixture.wait_messages(1).await?;
    let completed = next_event_value(&mut first).await?;
    assert_eq!(
        completed.pointer("/response/id"),
        Some(&Value::from("response-1"))
    );
    first.close(None).await.map_err(io::Error::other)?;

    let (mut second, second_status) = connect_local(&proxy).await?;
    assert_eq!(second_status, 101);
    send_thread_continuation(&mut second, "reconnecting-thread", "response-1").await?;
    server.fixture.wait_messages(2).await?;
    let continued = next_event_value(&mut second).await?;
    assert_eq!(
        continued.get("type"),
        Some(&Value::from("response.completed"))
    );
    assert_eq!(
        continued.pointer("/response/id"),
        Some(&Value::from("response-2"))
    );

    drop(second);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}
