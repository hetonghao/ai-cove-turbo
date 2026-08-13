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
    server.fixture.wait_ready(6).await?;

    send_continuation(&mut client, "resp_test").await?;
    let error = next_event_value(&mut client).await?;
    assert_eq!(error.get("type"), Some(&Value::from("error")));
    assert_eq!(
        error.pointer("/error/code"),
        Some(&Value::from("previous_response_not_found"))
    );
    assert_eq!(
        error.pointer("/error/message"),
        Some(&Value::from(
            "Previous response is not available on this websocket"
        ))
    );
    assert_counts(server.fixture.counts().await, 6, 0, 0);
    assert_eq!(metrics.snapshot().hybrid_ws, 0);
    assert!(metrics.traffic_snapshot().recent_requests.is_empty());
    let snapshot = proxy.connection_snapshot().await;
    assert_eq!(snapshot.current_connections, 6);
    assert_eq!(snapshot.prewarm, 6);

    client
        .send(Message::Ping(b"still-open".to_vec().into()))
        .await
        .map_err(io::Error::other)?;
    let Some(Ok(Message::Pong(_))) = client.next().await else {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "local websocket did not stay open",
        ));
    };
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
    server.fixture.wait_ready(6).await?;
    send_create(&mut client).await?;
    server.fixture.wait_messages(1).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    server.fixture.wait_close_frames(1).await?;
    server.fixture.wait_ready(7).await?;

    // When: the local session tries to continue the response on a replacement connection.
    send_continuation(&mut client, "response-1").await?;

    // Then: Turbo rejects the stale continuation locally without an upstream request or failure row.
    let error = next_event_value(&mut client).await?;
    assert_eq!(
        error.pointer("/error/code"),
        Some(&Value::from("previous_response_not_found"))
    );
    assert_counts_with_min_private(server.fixture.counts().await, 7, 1, 0);
    assert_eq!(metrics.snapshot().hybrid_ws, 1);
    let events = serde_json::to_value(metrics.traffic_snapshot().recent_requests)
        .map_err(io::Error::other)?;
    assert!(events.as_array().is_some_and(|events| {
        events
            .iter()
            .all(|event| event.get("failurePhase") != Some(&Value::from("hybridActive")))
    }));

    send_create(&mut client).await?;
    server.fixture.wait_messages(2).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");

    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn empty_recovery_payload_is_rejected_before_upstream() -> io::Result<()> {
    // Given: the recovered local session has no checked-out upstream connection.
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::Stateful,
        delay_http: false,
    })
    .await?;
    let (proxy, _) = start_test_proxy(&server).await?;
    let (mut client, status) = connect_local(&proxy).await?;
    assert_eq!(status, 101);
    server.fixture.wait_ready(6).await?;

    // When: Codex emits a recovery create without any continuation source.
    client
        .send(Message::Text(
            r#"{"type":"response.create","model":"test"}"#.into(),
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
    assert_counts(server.fixture.counts().await, 6, 0, 0);
    let snapshot = proxy.connection_snapshot().await;
    assert_eq!(snapshot.current_connections, 6);
    assert_eq!(snapshot.prewarm, 6);
    assert!(snapshot.bound_threads.is_empty());

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
    assert_counts(server.fixture.counts().await, 6, 0, 0);

    send_create(&mut client).await?;
    server.fixture.wait_messages(1).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    assert_counts_with_min_private(server.fixture.counts().await, 6, 1, 0);

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
    server.fixture.wait_ready(6).await?;

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
    server.fixture.wait_ready(7).await?;
    assert_counts(server.fixture.counts().await, 7, 3, 0);

    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}
