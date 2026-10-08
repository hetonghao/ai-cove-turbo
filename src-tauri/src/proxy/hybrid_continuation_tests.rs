use super::*;

async fn send_continuation(
    client: &mut ClientWebSocket,
    previous_response_id: &str,
) -> io::Result<()> {
    let request = serde_json::json!({
        "type": "response.create",
        "model": "test",
        "input": [],
        "previous_response_id": previous_response_id,
    });
    client
        .send(Message::Text(request.to_string().into()))
        .await
        .map_err(io::Error::other)
}

#[tokio::test]
async fn continuation_without_handoff_returns_local_state_missing() -> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::Stateful,
        delay_http: false,
    })
    .await?;
    let (proxy, metrics) = start_test_proxy(&server).await?;
    let (mut client, status) = connect_local(&proxy).await?;
    assert_eq!(status, 101);

    send_continuation(&mut client, "resp_test").await?;
    expect_missing_continuation_close(&mut client).await?;
    assert_counts(server.fixture.counts().await, 0, 0, 0);
    assert_eq!(metrics.snapshot().hybrid_ws, 0);
    assert!(metrics.traffic_snapshot().recent_requests.is_empty());
    let snapshot = proxy.connection_snapshot().await;
    assert_eq!(snapshot.current_connections, 0);
    assert_eq!(snapshot.prewarm, 0);
    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn stale_continuation_is_rejected_after_upstream_discard() -> io::Result<()> {
    // Given: a completed response whose upstream connection is discarded after an idle EOF.
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::IdleUnexpectedEof,
        delay_http: false,
    })
    .await?;
    let (proxy, metrics) = start_test_proxy(&server).await?;
    let (mut client, status) = connect_local(&proxy).await?;
    assert_eq!(status, 101);
    send_create(&mut client).await?;
    server.fixture.wait_messages(1).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    server.fixture.wait_close_frames(1).await?;

    // When: the local session tries to continue the response on a replacement connection.
    send_continuation(&mut client, "response-1").await?;

    // Then: Turbo rejects the stale continuation locally without an upstream request or failure row.
    expect_missing_continuation_close(&mut client).await?;
    assert_counts_with_min_private(server.fixture.counts().await, 1, 1, 0);
    assert_eq!(metrics.snapshot().hybrid_ws, 1);
    let events = serde_json::to_value(metrics.traffic_snapshot().recent_requests)
        .map_err(io::Error::other)?;
    assert!(events.as_array().is_some_and(|events| {
        events
            .iter()
            .all(|event| event.get("failurePhase") != Some(&Value::from("hybridActive")))
    }));

    // And: the reconnected client's full request remains usable.
    drop(client);
    let (mut client, status) = connect_local(&proxy).await?;
    assert_eq!(status, 101);
    send_create(&mut client).await?;
    server.fixture.wait_messages(2).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");

    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn empty_recovery_payload_is_rejected_and_reconnects_before_upstream() -> io::Result<()> {
    // Given: the recovered local session has no checked-out upstream connection.
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::Stateful,
        delay_http: false,
    })
    .await?;
    let (proxy, _) = start_test_proxy(&server).await?;
    let (mut client, status) = connect_local(&proxy).await?;
    assert_eq!(status, 101);

    // When: Codex emits a recovery create without any continuation source.
    client
        .send(Message::Text(
            r#"{"type":"response.create","model":"test","input":[]}"#.into(),
        ))
        .await
        .map_err(io::Error::other)?;

    // Then: Turbo rejects it locally and the next complete request remains usable.
    let error = next_event_value(&mut client).await?;
    assert_eq!(error.get("type"), Some(&Value::from("error")));
    assert_eq!(
        error.pointer("/error/code"),
        Some(&Value::from("previous_response_not_found"))
    );
    let close = tokio::time::timeout(Duration::from_secs(1), client.next())
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "empty recovery did not close"))?;
    let Some(Ok(Message::Close(Some(frame)))) = close else {
        return Err(io::Error::other(
            "empty recovery did not close local websocket",
        ));
    };
    assert_eq!(u16::from(frame.code), 1002);
    assert_counts(server.fixture.counts().await, 0, 0, 0);
    let snapshot = proxy.connection_snapshot().await;
    assert_eq!(snapshot.current_connections, 0);
    assert_eq!(snapshot.prewarm, 0);
    assert!(snapshot.bound_threads.is_empty());
    drop(client);

    let (mut client, status) = connect_local(&proxy).await?;
    assert_eq!(status, 101);

    // And: a malformed continuation id is not accepted as an upstream source.
    client
        .send(Message::Text(
            r#"{"type":"response.create","model":"test","previous_response_id":7}"#.into(),
        ))
        .await
        .map_err(io::Error::other)?;
    let error = next_event_value(&mut client).await?;
    assert_eq!(
        error.pointer("/error/code"),
        Some(&Value::from("previous_response_not_found"))
    );
    let close = tokio::time::timeout(Duration::from_secs(1), client.next())
        .await
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "malformed continuation did not close",
            )
        })?;
    let Some(Ok(Message::Close(Some(frame)))) = close else {
        return Err(io::Error::other(
            "malformed continuation did not close local websocket",
        ));
    };
    assert_eq!(u16::from(frame.code), 1002);
    assert_counts(server.fixture.counts().await, 0, 0, 0);
    drop(client);

    let (mut client, status) = connect_local(&proxy).await?;
    assert_eq!(status, 101);

    send_create(&mut client).await?;
    server.fixture.wait_messages(1).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    assert_counts_with_min_private(server.fixture.counts().await, 1, 1, 0);

    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn duplicate_terminal_tail_keeps_session_websocket_reusable() -> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::TerminalTail,
        delay_http: false,
    })
    .await?;
    let (proxy, _) = start_test_proxy(&server).await?;
    let (mut client, status) = connect_local(&proxy).await?;
    assert_eq!(status, 101);

    for expected in 1..=3 {
        send_create(&mut client).await?;
        server.fixture.wait_messages(expected).await?;
        let event = next_event_value(&mut client).await?;
        assert_eq!(event.get("type"), Some(&Value::from("response.completed")));
        assert_eq!(
            event.pointer("/response/id"),
            Some(&Value::from(format!("response-{expected}")))
        );
    }
    assert_counts(server.fixture.counts().await, 1, 3, 0);

    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}
