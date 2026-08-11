use super::*;

async fn send_continuation(client: &mut ClientWebSocket) -> io::Result<()> {
    client
        .send(Message::Text(
            r#"{"type":"response.create","model":"test","input":[],"previous_response_id":"resp_test"}"#.into(),
        ))
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

    send_continuation(&mut client).await?;
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
