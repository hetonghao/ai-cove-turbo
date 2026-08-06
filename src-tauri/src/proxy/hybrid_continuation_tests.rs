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
async fn continuation_waits_for_the_first_available_pooled_websocket() -> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::FailOnce,
        delay_http: false,
    })
    .await?;
    let (proxy, _) = start_test_proxy(&server).await?;
    let (mut client, status) = connect_local(&proxy).await?;
    assert_eq!(status, 101);

    send_continuation(&mut client).await?;
    server.fixture.wait_private(2).await?;
    server.fixture.wait_messages(1).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    assert_counts(server.fixture.counts().await, 2, 1, 0);

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
