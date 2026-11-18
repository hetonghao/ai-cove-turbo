use super::*;

const SOL: &str = "gpt-6.2-sol";
const ASTRA: &str = "gpt-6.2-astra";

#[tokio::test]
async fn direct_responses_adds_priority_only_for_the_enabled_model() -> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::Persistent,
        delay_http: false,
    })
    .await?;
    let (proxy, _) = start_test_proxy(&server).await?;
    let http = reqwest::Client::new();
    for (enabled, model, path, client_tier) in [
        (false, SOL, "responses", None),
        (true, SOL, "responses", Some("default")),
        (true, ASTRA, "responses", None),
        (true, SOL, "chat/completions", None),
        (false, SOL, "responses", Some("flex")),
    ] {
        proxy
            .set_model_fast(SOL.to_owned(), enabled)
            .map_err(io::Error::other)?;
        let mut request = serde_json::json!({"model": model, "input": "hello"});
        if let Some(tier) = client_tier {
            request["service_tier"] = Value::from(tier);
        }
        http.post(format!("{}/{path}", proxy.endpoint()))
            .header(header::CONTENT_TYPE, "application/json")
            .body(request.to_string())
            .send()
            .await
            .map_err(io::Error::other)?
            .error_for_status()
            .map_err(io::Error::other)?
            .bytes()
            .await
            .map_err(io::Error::other)?;
    }
    assert_tiers(
        &server.fixture.http_payloads().await,
        &[None, Some("priority"), None, None, Some("flex")],
    )?;
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn fast_toggle_applies_to_followups_on_the_same_websocket() -> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::Stateful,
        delay_http: false,
    })
    .await?;
    let (proxy, _) = start_test_proxy(&server).await?;
    let (mut client, _) = connect_local(&proxy).await?;

    let first = send_turn(&mut client, None).await?;
    proxy
        .set_model_fast(SOL.to_owned(), true)
        .map_err(io::Error::other)?;
    let second = send_turn(&mut client, Some(&first)).await?;
    proxy
        .set_model_fast(SOL.to_owned(), false)
        .map_err(io::Error::other)?;
    send_turn(&mut client, Some(&second)).await?;

    assert_tiers(
        &server.fixture.private_payloads().await,
        &[None, Some("priority"), None],
    )?;
    assert_eq!(server.fixture.counts().await.http_requests, 0);
    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn http_continuation_does_not_inherit_priority_when_fast_is_turned_off() -> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::Persistent,
        delay_http: false,
    })
    .await?;
    let (proxy, _) = start_test_proxy(&server).await?;
    proxy.set_capability_for_test(SOL, CapabilityTransport::HttpOnly);
    proxy
        .set_model_fast(SOL.to_owned(), true)
        .map_err(io::Error::other)?;
    let (mut client, _) = connect_local(&proxy).await?;

    let first = send_turn(&mut client, None).await?;
    proxy
        .set_model_fast(SOL.to_owned(), false)
        .map_err(io::Error::other)?;
    send_turn(&mut client, Some(&first)).await?;

    assert_tiers(
        &server.fixture.http_payloads().await,
        &[Some("priority"), None],
    )?;
    assert_eq!(server.fixture.counts().await.private_messages, 0);
    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn safe_http_fallback_retains_the_websocket_request_tier() -> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::ActiveTransportFallback,
        delay_http: false,
    })
    .await?;
    let (proxy, _) = start_test_proxy(&server).await?;
    proxy.set_capability_for_test(SOL, CapabilityTransport::WebSocket);
    let (mut client, _) = connect_local(&proxy).await?;

    send_turn(&mut client, None).await?;
    proxy
        .set_model_fast(SOL.to_owned(), true)
        .map_err(io::Error::other)?;
    send_turn(&mut client, None).await?;

    assert_tiers(
        &server.fixture.private_payloads().await,
        &[None, Some("priority")],
    )?;
    assert_tiers(&server.fixture.http_payloads().await, &[Some("priority")])?;
    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn private_legacy_relay_applies_fast_to_response_create_frames_only() -> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::Persistent,
        delay_http: false,
    })
    .await?;
    let (proxy, _) = start_test_proxy(&server).await?;
    proxy
        .set_model_fast(SOL.to_owned(), true)
        .map_err(io::Error::other)?;
    let (mut client, _) = connect_local(&proxy).await?;

    client
        .send(Message::Text(
            r#"{"type":"session.update","model":"gpt-6.2-sol"}"#.into(),
        ))
        .await
        .map_err(io::Error::other)?;
    tokio::time::timeout(Duration::from_secs(4), next_event_value(&mut client)).await??;
    send_turn(&mut client, None).await?;
    proxy
        .set_model_fast(SOL.to_owned(), false)
        .map_err(io::Error::other)?;
    send_turn(&mut client, None).await?;

    assert_tiers(
        &server.fixture.private_payloads().await,
        &[None, Some("priority"), None],
    )?;
    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

async fn send_turn(client: &mut ClientWebSocket, previous: Option<&str>) -> io::Result<String> {
    let request = if let Some(id) = previous {
        serde_json::json!({"type": "response.create", "previous_response_id": id, "input": []})
    } else {
        serde_json::json!({"type": "response.create", "model": SOL, "input": [{"type": "message", "role": "user", "content": "hello"}]})
    };
    client
        .send(Message::Text(request.to_string().into()))
        .await
        .map_err(io::Error::other)?;
    let event = tokio::time::timeout(Duration::from_secs(3), next_event_value(client)).await??;
    assert_eq!(
        event.get("type").and_then(Value::as_str),
        Some("response.completed"),
        "{event}"
    );
    event
        .pointer("/response/id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| io::Error::other("missing response ID"))
}

fn assert_tiers(payloads: &[Vec<u8>], expected: &[Option<&str>]) -> io::Result<()> {
    let decoded = payloads
        .iter()
        .map(|payload| serde_json::from_slice::<Value>(payload))
        .collect::<Result<Vec<_>, _>>()
        .map_err(io::Error::other)?;
    let actual = decoded
        .iter()
        .map(|payload| payload.get("service_tier").and_then(Value::as_str))
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
    Ok(())
}
