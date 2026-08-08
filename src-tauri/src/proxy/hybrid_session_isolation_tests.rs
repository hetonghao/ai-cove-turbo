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
    server.fixture.wait_ready(7).await?;

    send_thread_create(&mut client, "parent-thread").await?;
    server.fixture.wait_messages(1).await?;
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
    server.fixture.wait_ready(7).await?;

    send_thread_create(&mut client, "cloned-thread").await?;
    server.fixture.wait_messages(1).await?;
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
    server.fixture.wait_private(7).await?;

    send_thread_create(&mut client, "parent-thread").await?;
    server.fixture.wait_http(1).await?;
    send_thread_create(&mut client, "cloned-thread").await?;
    assert_eq!(next_event_type(&mut client).await?, "error");
    expect_protocol_close(&mut client).await?;
    assert_counts(server.fixture.counts().await, 7, 0, 1);

    server.fixture.release_http();
    for _ in 0..7 {
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
    server.fixture.wait_ready(7).await?;

    send_thread_create(&mut client, "parent-thread").await?;
    server.fixture.wait_messages(1).await?;
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
