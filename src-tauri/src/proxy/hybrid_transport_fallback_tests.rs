use super::*;

#[tokio::test]
async fn active_ws_not_submitted_fallback_completes_over_http_on_same_client() -> io::Result<()> {
    // Given: one request has completed on a ready private WebSocket.
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::ActiveTransportFallback,
        delay_http: false,
    })
    .await?;
    let (proxy, metrics) = start_test_proxy(&server).await?;
    let (mut client, status) = connect_local(&proxy).await?;
    assert_eq!(status, 101);
    server.fixture.wait_ready(6).await?;
    send_create(&mut client).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");

    // When: New API proves that the next WS request was not submitted.
    send_create(&mut client).await?;
    server.fixture.wait_messages(2).await?;

    // Then: Turbo consumes the transport error and completes exactly one HTTP request.
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    server.fixture.wait_http(1).await?;
    assert_counts_with_min_private(server.fixture.counts().await, 6, 2, 1);
    client
        .send(Message::Ping(b"same-client".to_vec().into()))
        .await
        .map_err(io::Error::other)?;
    let Some(Ok(Message::Pong(_))) = client.next().await else {
        return Err(io::Error::other("local websocket did not survive fallback"));
    };
    let events = serde_json::to_value(metrics.traffic_snapshot().recent_requests)
        .map_err(io::Error::other)?;
    assert!(
        events
            .as_array()
            .is_some_and(|events| events.iter().any(|event| {
                event.get("route") == Some(&Value::from("hybridRecoveryHttp"))
                    && event.get("result") == Some(&Value::from("fallback"))
            }))
    );

    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn active_ws_fallback_after_output_does_not_replay_over_http() -> io::Result<()> {
    // Given: a private WebSocket has completed one request.
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::ActiveOutputThenTransportFallback,
        delay_http: false,
    })
    .await?;
    let (proxy, _) = start_test_proxy(&server).await?;
    let (mut client, _) = connect_local(&proxy).await?;
    server.fixture.wait_ready(6).await?;
    send_create(&mut client).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");

    // When: output arrives before the otherwise valid fallback contract.
    send_create(&mut client).await?;
    assert_eq!(
        next_event_type(&mut client).await?,
        "response.output_text.delta"
    );

    // Then: Turbo forwards the error and never replays the partial response over HTTP.
    assert_eq!(next_event_type(&mut client).await?, "error");
    assert_counts_with_min_private(server.fixture.counts().await, 6, 2, 0);

    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn active_ws_generic_error_with_fallback_text_does_not_replay_over_http() -> io::Result<()> {
    // Given: a private WebSocket returns the same human message as the fallback contract.
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::ActiveGenericFallbackLookalike,
        delay_http: false,
    })
    .await?;
    let (proxy, _) = start_test_proxy(&server).await?;
    let (mut client, _) = connect_local(&proxy).await?;
    server.fixture.wait_ready(6).await?;
    send_create(&mut client).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");

    // When: the next request fails without the dedicated machine-readable code.
    send_create(&mut client).await?;

    // Then: Turbo forwards the generic error and performs no HTTP replay.
    assert_eq!(next_event_type(&mut client).await?, "error");
    assert_counts_with_min_private(server.fixture.counts().await, 6, 2, 0);

    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn active_ws_continuation_fallback_contract_does_not_replay_over_http() -> io::Result<()> {
    // Given: a private WebSocket has established response state.
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::ActiveTransportFallback,
        delay_http: false,
    })
    .await?;
    let (proxy, _) = start_test_proxy(&server).await?;
    let (mut client, _) = connect_local(&proxy).await?;
    server.fixture.wait_ready(6).await?;
    send_create(&mut client).await?;
    let completed = next_event_value(&mut client).await?;
    assert_eq!(
        completed.pointer("/response/id"),
        Some(&Value::from("response-1"))
    );

    // When: a continuation receives the fallback contract.
    client
        .send(Message::Text(
            r#"{"type":"response.create","model":"test","previous_response_id":"response-1","input":"next"}"#.into(),
        ))
        .await
        .map_err(io::Error::other)?;

    // Then: the stateful request remains WebSocket-required and is not sent over HTTP.
    assert_eq!(next_event_type(&mut client).await?, "error");
    assert_counts_with_min_private(server.fixture.counts().await, 6, 2, 0);

    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn active_ws_cancel_before_fallback_contract_does_not_restart_over_http() -> io::Result<()> {
    // Given: a second private WebSocket request is active.
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::ActiveCancelThenTransportFallback,
        delay_http: false,
    })
    .await?;
    let (proxy, _) = start_test_proxy(&server).await?;
    let (mut client, _) = connect_local(&proxy).await?;
    server.fixture.wait_ready(6).await?;
    send_create(&mut client).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    send_create(&mut client).await?;
    server.fixture.wait_messages(2).await?;

    // When: cancellation wins before New API emits the fallback contract.
    send_cancel(&mut client).await?;
    server.fixture.wait_messages(3).await?;

    // Then: Turbo forwards the terminal error without restarting the cancelled request.
    assert_eq!(next_event_type(&mut client).await?, "error");
    assert_counts_with_min_private(server.fixture.counts().await, 6, 3, 0);

    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn active_ws_cancel_race_with_fallback_contract_does_not_replay_over_http() -> io::Result<()>
{
    // Given: the upstream emits the fallback contract as soon as the active request arrives.
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::ActiveTransportFallback,
        delay_http: false,
    })
    .await?;
    let (proxy, _) = start_test_proxy(&server).await?;
    let (mut client, _) = connect_local(&proxy).await?;
    server.fixture.wait_ready(6).await?;
    send_create(&mut client).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");

    // When: cancellation races the already-queued fallback contract.
    send_create(&mut client).await?;
    send_cancel(&mut client).await?;

    // Then: Turbo treats the cancellation as a hard no-replay boundary.
    assert_eq!(next_event_type(&mut client).await?, "error");
    assert_counts_with_min_private(server.fixture.counts().await, 6, 2, 0);

    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn failed_http_fallback_does_not_start_another_transport_attempt() -> io::Result<()> {
    // Given: a ready private WebSocket later emits a safe fallback contract.
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::ActiveTransportFallbackHttpFailure,
        delay_http: false,
    })
    .await?;
    let (proxy, metrics) = start_test_proxy(&server).await?;
    let (mut client, _) = connect_local(&proxy).await?;
    server.fixture.wait_ready(6).await?;
    send_create(&mut client).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");

    // When: the single HTTP fallback returns 413.
    send_create(&mut client).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.failed");

    // Then: the HTTP attempt is terminal for transport recovery and the local WS stays open.
    assert_counts_with_min_private(server.fixture.counts().await, 6, 2, 1);
    let events = serde_json::to_value(metrics.traffic_snapshot().recent_requests)
        .map_err(io::Error::other)?;
    assert!(
        events
            .as_array()
            .is_some_and(|events| events.iter().any(|event| {
                event.get("route") == Some(&Value::from("hybridRecoveryHttp"))
                    && event.get("status") == Some(&Value::from(413))
                    && event.get("result") == Some(&Value::from("error"))
            }))
    );
    client
        .send(Message::Ping(b"fallback-failed".to_vec().into()))
        .await
        .map_err(io::Error::other)?;
    let Some(Ok(Message::Pong(_))) = client.next().await else {
        return Err(io::Error::other(
            "local websocket closed after HTTP fallback failure",
        ));
    };

    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}
