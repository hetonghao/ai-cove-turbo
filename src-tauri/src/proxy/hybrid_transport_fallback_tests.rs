use super::*;

#[tokio::test]
async fn capability_shutdown_stops_pending_handshake_retries() -> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::Fail,
        delay_http: false,
    })
    .await?;
    let (proxy, _) = start_test_proxy(&server).await?;
    proxy.set_capability_for_test("test", CapabilityTransport::WebSocket);
    let (mut client, _) = connect_local(&proxy).await?;
    send_create(&mut client).await?;
    server.fixture.wait_private(1).await?;
    proxy.set_capability_for_test("test", CapabilityTransport::HttpOnly);
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert_counts(server.fixture.counts().await, 1, 0, 1);
    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn http_model_does_not_prewarm_or_close_another_models_ws() -> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::Persistent,
        delay_http: false,
    })
    .await?;
    let (proxy, _) = start_test_proxy(&server).await?;
    proxy.set_capability_for_test("test", CapabilityTransport::WebSocket);
    proxy.set_capability_for_test("http-model", CapabilityTransport::HttpOnly);
    let (mut ws_client, _) = connect_local(&proxy).await?;
    let (mut http_client, _) = connect_local(&proxy).await?;
    send_create(&mut ws_client).await?;
    assert_eq!(next_event_type(&mut ws_client).await?, "response.completed");
    http_client
        .send(Message::Text(
            serde_json::json!({
                "type":"response.create", "model":"http-model", "input":[], "generate":false,
            })
            .to_string()
            .into(),
        ))
        .await
        .map_err(io::Error::other)?;
    assert_eq!(next_event_type(&mut http_client).await?, "response.created");
    assert_eq!(
        next_event_type(&mut http_client).await?,
        "response.completed"
    );
    send_create(&mut ws_client).await?;
    assert_eq!(next_event_type(&mut ws_client).await?, "response.completed");
    assert_counts(server.fixture.counts().await, 1, 2, 0);
    drop(ws_client);
    drop(http_client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn warmup_safe_fallback_does_not_submit_http_generation() -> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::ActiveTransportFallback,
        delay_http: false,
    })
    .await?;
    let (proxy, _) = start_test_proxy(&server).await?;
    let (mut client, _) = connect_local(&proxy).await?;
    send_create(&mut client).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    client.send(Message::Text(serde_json::json!({
        "type":"response.create", "model":"test", "input":[{"role":"user","content":"context"}], "generate":false,
    }).to_string().into())).await.map_err(io::Error::other)?;
    assert_eq!(next_event_type(&mut client).await?, "response.created");
    let completed = next_event_value(&mut client).await?;
    assert_eq!(
        completed.get("type"),
        Some(&Value::from("response.completed"))
    );
    assert_counts(server.fixture.counts().await, 1, 2, 0);
    client.send(Message::Text(serde_json::json!({
        "type":"response.create", "model":"test", "input":[], "previous_response_id":completed.pointer("/response/id"),
    }).to_string().into())).await.map_err(io::Error::other)?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    assert_counts(server.fixture.counts().await, 1, 2, 1);
    let payloads = server.fixture.http_payloads().await;
    let sent: Value = serde_json::from_slice(
        payloads
            .first()
            .ok_or_else(|| io::Error::other("HTTP payload missing"))?,
    )
    .map_err(io::Error::other)?;
    assert_eq!(
        sent.get("input"),
        Some(&serde_json::json!([{"role":"user","content":"context"}]))
    );
    assert!(sent.get("generate").is_none());
    assert!(sent.get("previous_response_id").is_none());
    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn capability_shutdown_during_response_finishes_without_replay() -> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::HoldResponse,
        delay_http: false,
    })
    .await?;
    let (proxy, _) = start_test_proxy(&server).await?;
    proxy.set_capability_for_test("test", CapabilityTransport::WebSocket);
    let (mut client, _) = connect_local(&proxy).await?;
    send_create(&mut client).await?;
    server.fixture.wait_active_ready(1).await?;
    proxy.set_capability_for_test("test", CapabilityTransport::HttpOnly);
    server.fixture.release_private();
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    server.fixture.wait_normal_closes(1).await?;
    assert_counts(server.fixture.counts().await, 1, 1, 0);
    send_create(&mut client).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    assert_counts(server.fixture.counts().await, 1, 1, 1);
    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn capability_shutdown_releases_idle_ws_without_refill_and_routes_next_turn_http()
-> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::Persistent,
        delay_http: false,
    })
    .await?;
    let (proxy, _) = start_test_proxy(&server).await?;
    proxy.set_capability_for_test("test", CapabilityTransport::WebSocket);
    let (mut client, _) = connect_local(&proxy).await?;
    assert_counts(server.fixture.counts().await, 0, 0, 0);
    send_create(&mut client).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    assert_counts(server.fixture.counts().await, 1, 1, 0);
    proxy.set_capability_for_test("test", CapabilityTransport::HttpOnly);
    server.fixture.wait_normal_closes(1).await?;
    send_create(&mut client).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert_counts(server.fixture.counts().await, 1, 1, 1);
    assert_eq!(proxy.connection_snapshot().await.current_connections, 0);
    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn http_warmup_completes_locally_and_preserves_incremental_input() -> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::Persistent,
        delay_http: false,
    })
    .await?;
    let (proxy, _) = start_test_proxy(&server).await?;
    proxy.set_capability_for_test("test", CapabilityTransport::HttpOnly);
    let (mut client, _) = connect_local(&proxy).await?;
    for input in [
        serde_json::json!([]),
        serde_json::json!([{"role":"user","content":"warmup context"}]),
    ] {
        client
            .send(Message::Text(
                serde_json::json!({
                    "type":"response.create", "model":"test", "generate":false,
                    "instructions":"keep this", "input":input,
                })
                .to_string()
                .into(),
            ))
            .await
            .map_err(io::Error::other)?;
        let created = next_event_value(&mut client).await?;
        assert_eq!(created.get("type"), Some(&Value::from("response.created")));
        let completed = next_event_value(&mut client).await?;
        assert_eq!(
            completed.get("type"),
            Some(&Value::from("response.completed"))
        );
        assert_eq!(
            completed.pointer("/response/usage/total_tokens"),
            Some(&Value::from(0))
        );
        assert_eq!(
            completed.pointer("/response/output"),
            Some(&serde_json::json!([]))
        );
        assert_counts(server.fixture.counts().await, 0, 0, 0);
    }
    // A fresh warmup retains its own context, and only the subsequent generation reaches HTTP.
    client
        .send(Message::Text(
            serde_json::json!({
                "type":"response.create", "model":"test", "generate":false,
                "input":[{"role":"user","content":"context"}], "instructions":"keep this",
            })
            .to_string()
            .into(),
        ))
        .await
        .map_err(io::Error::other)?;
    let _ = next_event_value(&mut client).await?;
    let completed = next_event_value(&mut client).await?;
    client.send(Message::Text(serde_json::json!({
        "type":"response.create", "model":"test", "previous_response_id":completed.pointer("/response/id"),
        "input":[{"role":"user","content":"generate now"}],
    }).to_string().into())).await.map_err(io::Error::other)?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    assert_counts(server.fixture.counts().await, 0, 0, 1);
    let payloads = server.fixture.http_payloads().await;
    let sent: Value = serde_json::from_slice(
        payloads
            .first()
            .ok_or_else(|| io::Error::other("HTTP payload missing"))?,
    )
    .map_err(io::Error::other)?;
    assert_eq!(
        sent.get("input"),
        Some(&serde_json::json!([
            {"role":"user","content":"context"}, {"role":"user","content":"generate now"}
        ]))
    );
    assert_eq!(sent.get("instructions"), Some(&Value::from("keep this")));
    assert!(sent.get("generate").is_none());
    assert!(sent.get("previous_response_id").is_none());
    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn explicit_http_policy_takes_precedence_over_websocket_capability() -> io::Result<()> {
    // Given: an explicit HTTP policy and a cached WebSocket capability.
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::Persistent,
        delay_http: false,
    })
    .await?;
    let directory = tempfile::tempdir()?;
    let policy_path = directory.path().join("policy.json");
    std::fs::write(
        &policy_path,
        br#"{"version":1,"default_transport":"auto","models":{"test":{"transport":"http"}}}"#,
    )?;
    let (proxy, metrics) = start_test_proxy_with_policy(&server, Some(policy_path)).await?;
    proxy.set_capability_for_test("test", CapabilityTransport::WebSocket);
    let (mut client, status) = connect_local(&proxy).await?;
    assert_eq!(status, 101);

    // When: the first response is submitted.
    send_transcript_create(&mut client).await?;
    server.fixture.wait_http(1).await?;

    // Then: Turbo uses policy HTTP and never sends an upstream WS application frame.
    let first = next_event_value(&mut client).await?;
    let first_id = first
        .pointer("/response/id")
        .and_then(Value::as_str)
        .ok_or_else(|| io::Error::other("HTTP response id missing"))?;
    send_tool_output_continuation(&mut client, first_id, "call-fixture-1").await?;
    server.fixture.wait_http(2).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    let counts = server.fixture.counts().await;
    assert_eq!(counts.private_handshakes, 0);
    assert_eq!(counts.private_messages, 0);
    assert_eq!(counts.http_requests, 2);
    assert_eq!(metrics.snapshot().hybrid_policy_http, 2);
    assert_eq!(metrics.snapshot().hybrid_capability_http, 0);

    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn auto_http_only_capability_skips_upstream_websocket_application_attempt() -> io::Result<()>
{
    // Given: Auto policy and a cached HttpOnly capability for the requested model.
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::Persistent,
        delay_http: false,
    })
    .await?;
    let (proxy, metrics) = start_test_proxy(&server).await?;
    proxy.set_capability_for_test("test", CapabilityTransport::HttpOnly);
    let (mut client, status) = connect_local(&proxy).await?;
    assert_eq!(status, 101);

    // When: the first independent response is submitted with Auto policy.
    send_create(&mut client).await?;
    server.fixture.wait_http(1).await?;

    // Then: Turbo uses the existing HTTP worker without an upstream WS application frame.
    let completed = next_event_value(&mut client).await?;
    assert_eq!(
        completed.get("type"),
        Some(&Value::from("response.completed"))
    );
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert_counts(server.fixture.counts().await, 0, 0, 1);
    let routes = serde_json::to_value(metrics.traffic_snapshot().recent_requests)
        .map_err(io::Error::other)?;
    assert!(routes.as_array().is_some_and(|events| {
        events
            .iter()
            .any(|event| event.get("route") == Some(&Value::from("hybridCapabilityHttp")))
    }));

    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn capability_http_continuation_expands_previous_transcript_over_http() -> io::Result<()> {
    // Given: Auto policy resolves the model to HttpOnly and the first HTTP turn emits a tool call.
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::Persistent,
        delay_http: false,
    })
    .await?;
    let (proxy, metrics) = start_test_proxy(&server).await?;
    proxy.set_capability_for_test("test", CapabilityTransport::HttpOnly);
    let (mut client, _) = connect_local(&proxy).await?;
    send_transcript_create(&mut client).await?;
    let first = next_event_value(&mut client).await?;
    let first_id = first
        .pointer("/response/id")
        .and_then(Value::as_str)
        .ok_or_else(|| io::Error::other("HTTP response id missing"))?;
    assert_eq!(first_id, "http-response-1");

    // When: Codex sends only the tool output with previous_response_id.
    send_tool_output_continuation(&mut client, first_id, "call-fixture-1").await?;

    // Then: Turbo sends a self-contained HTTP request instead of waiting for a client resend.
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    let payloads = server.fixture.http_payloads().await;
    assert_eq!(payloads.len(), 2);
    let continuation: Value = serde_json::from_slice(
        payloads
            .get(1)
            .ok_or_else(|| io::Error::other("second HTTP request missing"))?,
    )
    .map_err(io::Error::other)?;
    assert!(continuation.get("previous_response_id").is_none());
    assert_eq!(
        continuation.pointer("/input/1/call_id"),
        Some(&Value::from("call-fixture-1"))
    );
    let counts = server.fixture.counts().await;
    assert_eq!(counts.private_messages, 0);
    assert_eq!(metrics.snapshot().hybrid_capability_http, 2);

    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn policy_http_continuation_expands_previous_transcript_over_http() -> io::Result<()> {
    // Given: 用户策略把模型固定为 HTTP，且第一轮已经产出 tool call。
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::Persistent,
        delay_http: false,
    })
    .await?;
    let directory = tempfile::tempdir()?;
    let policy_path = directory.path().join("policy.json");
    std::fs::write(
        &policy_path,
        br#"{"version":1,"default_transport":"auto","models":{"test":{"transport":"http"}}}"#,
    )?;
    let (proxy, _) = start_test_proxy_with_policy(&server, Some(policy_path)).await?;
    let (mut client, status) = connect_local(&proxy).await?;
    assert_eq!(status, 101);
    send_transcript_create(&mut client).await?;
    let first = next_event_value(&mut client).await?;
    let first_id = first
        .pointer("/response/id")
        .and_then(Value::as_str)
        .ok_or_else(|| io::Error::other("HTTP response id missing"))?;
    assert_eq!(first_id, "http-response-1");

    // When: 客户端只回带工具输出的增量续传帧。
    send_tool_output_continuation(&mut client, first_id, "call-fixture-1").await?;

    // Then: 第二条请求本身自包含，不需要上游解析任何状态。
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    let payloads = server.fixture.http_payloads().await;
    assert_eq!(payloads.len(), 2);
    let upstream_payload = payloads
        .get(1)
        .ok_or_else(|| io::Error::other("second HTTP request missing"))?;
    let continuation: Value = serde_json::from_slice(upstream_payload).map_err(io::Error::other)?;
    assert!(continuation.get("previous_response_id").is_none());
    assert!(continuation.get("type").is_none());
    assert_eq!(continuation.get("stream"), Some(&Value::Bool(true)));
    assert_eq!(
        continuation.get("instructions"),
        Some(&Value::from("be brief"))
    );
    let item_types: Vec<&str> = continuation
        .get("input")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("type").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        item_types,
        vec!["message", "custom_tool_call", "custom_tool_call_output"]
    );
    assert_eq!(
        continuation.pointer("/input/1/call_id"),
        Some(&Value::from("call-fixture-1"))
    );

    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn policy_http_continuation_without_pairing_rejects_locally() -> io::Result<()> {
    // Given: 用户策略固定 HTTP，第一轮产出 `call-fixture-1`。
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::Persistent,
        delay_http: false,
    })
    .await?;
    let directory = tempfile::tempdir()?;
    let policy_path = directory.path().join("policy.json");
    std::fs::write(
        &policy_path,
        br#"{"version":1,"default_transport":"auto","models":{"test":{"transport":"http"}}}"#,
    )?;
    let (proxy, _) = start_test_proxy_with_policy(&server, Some(policy_path)).await?;
    let (mut client, _) = connect_local(&proxy).await?;
    send_transcript_create(&mut client).await?;
    let first = next_event_value(&mut client).await?;
    let first_id = first
        .pointer("/response/id")
        .and_then(Value::as_str)
        .ok_or_else(|| io::Error::other("HTTP response id missing"))?;

    // When: 续传帧引用了一个上一轮并不存在的 call_id。
    send_tool_output_continuation(&mut client, first_id, "call-unknown").await?;

    // Then: 不做无法自证的展开，本地报告状态缺失并关闭连接，也不再占用一次上游请求。
    expect_missing_continuation_close(&mut client).await?;
    assert_eq!(server.fixture.counts().await.http_requests, 1);

    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn active_ws_not_submitted_fallback_completes_over_http_on_same_client() -> io::Result<()> {
    // Given: one request has completed on a ready private WebSocket.
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::ActiveTransportFallback,
        delay_http: false,
    })
    .await?;
    let (proxy, metrics) = start_test_proxy(&server).await?;
    proxy.set_capability_for_test("gpt-5.3-codex", CapabilityTransport::WebSocket);
    let (mut client, status) = connect_local(&proxy).await?;
    assert_eq!(status, 101);
    send_create_with_metadata(&mut client).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");

    // When: New API proves that the next WS request was not submitted.
    send_create_with_metadata(&mut client).await?;
    server.fixture.wait_messages(2).await?;

    // Then: Turbo consumes the transport error and completes exactly one HTTP request.
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    server.fixture.wait_http(1).await?;
    assert_counts_with_min_private(server.fixture.counts().await, 1, 2, 1);
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
                    && event.get("model") == Some(&Value::from("gpt-5.3-codex"))
                    && event.get("threadId") == Some(&Value::from("thread-123"))
                    && event.get("sessionId") == Some(&Value::from("session-123"))
            }))
    );

    // And: explicit unavailability makes the next independent Auto request use HTTP
    // without another WS application attempt; the first fallback remains historical fact.
    let input = "x".repeat(128 * 1024);
    client
        .send(Message::Text(
            serde_json::json!({
                "type":"response.create", "model":"gpt-5.3-codex", "input":input,
                "client_metadata":{"session_id":"session-123", "thread_id":"thread-123"},
            })
            .to_string()
            .into(),
        ))
        .await
        .map_err(io::Error::other)?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    assert_eq!(server.fixture.counts().await.http_requests, 2);
    assert_eq!(server.fixture.counts().await.private_messages, 2);
    assert_eq!(
        proxy
            .capability_statuses()
            .get("gpt-5.3-codex")
            .map(|status| status.transport),
        Some(CapabilityTransport::HttpOnly)
    );
    let snapshot = metrics.snapshot();
    assert_eq!(snapshot.hybrid_recovery_http, 1);
    assert_eq!(snapshot.hybrid_capability_http, 1);
    assert_eq!(snapshot.hybrid_ws, 1);
    assert_eq!(
        server
            .fixture
            .state
            .http_encodings
            .lock()
            .await
            .last()
            .cloned(),
        Some(Some("zstd".to_owned()))
    );
    let bodies = server.fixture.http_payloads().await;
    let compressed = bodies
        .last()
        .ok_or_else(|| io::Error::other("compressed HTTP request missing"))?;
    let sent: Value = serde_json::from_slice(&zstd::stream::decode_all(compressed.as_slice())?)
        .map_err(io::Error::other)?;
    assert_eq!(
        sent.get("input").and_then(Value::as_str),
        Some(input.as_str())
    );
    assert!(sent.get("type").is_none());
    let (mut next_session, _) = connect_local(&proxy).await?;
    send_create_with_metadata(&mut next_session).await?;
    assert_eq!(
        next_event_type(&mut next_session).await?,
        "response.completed"
    );
    assert_eq!(server.fixture.counts().await.private_messages, 2);
    assert_eq!(server.fixture.counts().await.http_requests, 3);
    drop(next_session);
    let events = serde_json::to_value(metrics.traffic_snapshot().recent_requests)
        .map_err(io::Error::other)?;
    assert!(
        events
            .as_array()
            .is_some_and(|events| events.iter().any(|event| {
                event.get("route") == Some(&Value::from("hybridCapabilityHttp"))
                    && event.get("result") == Some(&Value::from("success"))
            }))
    );

    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn http_continuation_after_fallback_records_capability_http_without_rewriting_history()
-> io::Result<()> {
    for restore_in_another_session in [false, true] {
        let server = FixtureServer::start(FixtureConfig {
            private: PrivateBehavior::ActiveTransportFallback,
            delay_http: false,
        })
        .await?;
        let (proxy, metrics) = start_test_proxy(&server).await?;
        proxy.set_capability_for_test("test", CapabilityTransport::WebSocket);
        let (mut client, _) = connect_local(&proxy).await?;
        send_transcript_create(&mut client).await?;
        assert_eq!(next_event_type(&mut client).await?, "response.completed");
        send_transcript_create(&mut client).await?;
        let fallback = next_event_value(&mut client).await?;
        assert_eq!(
            fallback.get("type"),
            Some(&Value::from("response.completed"))
        );
        let response_id = fallback
            .pointer("/response/id")
            .and_then(Value::as_str)
            .ok_or_else(|| io::Error::other("HTTP fallback response missing"))?;
        assert_eq!(metrics.snapshot().hybrid_recovery_http, 1);

        if restore_in_another_session {
            *server.fixture.state.capability_websocket.lock().await = Some(true);
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(31)).await;
            tokio::time::resume();
            let mut changed = proxy.capability_changes();
            let (mut other, _) = connect_local(&proxy).await?;
            send_transcript_create(&mut other).await?;
            assert_eq!(next_event_type(&mut other).await?, "response.completed");
            server.fixture.wait_capabilities(1).await?;
            tokio::time::timeout(Duration::from_secs(1), changed.changed())
                .await
                .map_err(io::Error::other)?
                .map_err(io::Error::other)?;
            assert_eq!(
                proxy
                    .capability_statuses()
                    .get("test")
                    .map(|status| status.transport),
                Some(CapabilityTransport::WebSocket)
            );
            // B proves WS is actually usable again; A still owns HTTP continuation state.
            send_transcript_create(&mut other).await?;
            assert_eq!(next_event_type(&mut other).await?, "response.completed");
            assert_eq!(server.fixture.counts().await.private_messages, 3);
            drop(other);
        }
        // This is a new HTTP turn referencing the completed fallback, not a new WS failure.
        send_tool_output_continuation(&mut client, response_id, "call-fixture-1").await?;
        assert_eq!(next_event_type(&mut client).await?, "response.completed");
        assert_eq!(
            server.fixture.counts().await.private_messages,
            if restore_in_another_session { 3 } else { 2 }
        );
        assert_eq!(
            server.fixture.counts().await.http_requests,
            if restore_in_another_session { 3 } else { 2 }
        );
        assert_eq!(metrics.snapshot().hybrid_recovery_http, 1);
        assert_eq!(
            metrics.snapshot().hybrid_capability_http,
            if restore_in_another_session { 2 } else { 1 }
        );
        let events = serde_json::to_value(metrics.traffic_snapshot().recent_requests)
            .map_err(io::Error::other)?;
        let events = events
            .as_array()
            .ok_or_else(|| io::Error::other("request records missing"))?;
        assert_eq!(
            events
                .iter()
                .filter(
                    |event| event.get("route") == Some(&Value::from("hybridRecoveryHttp"))
                        && event.get("result") == Some(&Value::from("fallback"))
                )
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(
                    |event| event.get("route") == Some(&Value::from("hybridCapabilityHttp"))
                        && event.get("result") == Some(&Value::from("success"))
                )
                .count(),
            if restore_in_another_session { 2 } else { 1 }
        );
        drop(client);
        proxy.stop().await;
        server.stop().await;
    }
    Ok(())
}

#[tokio::test]
async fn fallback_does_not_borrow_or_poison_another_credentials_capability() -> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::ActiveTransportFallback,
        delay_http: false,
    })
    .await?;
    let (proxy, _) = start_test_proxy(&server).await?;
    proxy.set_capability_for_test("gpt-5.3-codex", CapabilityTransport::WebSocket);
    let (mut first, _) = connect_local(&proxy).await?;
    send_create_with_metadata(&mut first).await?;
    assert_eq!(next_event_type(&mut first).await?, "response.completed");
    send_create_with_metadata(&mut first).await?;
    assert_eq!(next_event_type(&mut first).await?, "response.completed");

    // A different actual forwarded credential has no right to use the startup
    // credential's cached WS or negative evidence. It first uses HTTP and probes itself.
    *server.fixture.state.capability_websocket.lock().await = Some(true);
    let (mut other, _) = connect_local_with_authorization(&proxy, Some("Bearer other-key")).await?;
    send_create_with_metadata(&mut other).await?;
    assert_eq!(next_event_type(&mut other).await?, "response.completed");
    assert_eq!(server.fixture.counts().await.private_messages, 2);
    server.fixture.wait_capabilities(1).await?;
    assert_eq!(
        server
            .fixture
            .state
            .capability_headers
            .lock()
            .await
            .first()
            .and_then(|headers| headers.get(header::AUTHORIZATION))
            .and_then(|value| value.to_str().ok()),
        Some("Bearer other-key")
    );
    send_create_with_metadata(&mut first).await?;
    assert_eq!(next_event_type(&mut first).await?, "response.completed");
    assert_eq!(server.fixture.counts().await.private_messages, 2);
    // The same model on a different platform has its own transport evidence.
    let other_platform = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::Persistent,
        delay_http: false,
    })
    .await?;
    let (other_proxy, _) = start_test_proxy(&other_platform).await?;
    other_proxy.set_capability_for_test("gpt-5.3-codex", CapabilityTransport::WebSocket);
    let (mut platform_client, _) = connect_local(&other_proxy).await?;
    send_create_with_metadata(&mut platform_client).await?;
    assert_eq!(
        next_event_type(&mut platform_client).await?,
        "response.completed"
    );
    assert_eq!(other_platform.fixture.counts().await.private_messages, 1);
    assert_eq!(other_platform.fixture.counts().await.http_requests, 0);
    drop(platform_client);
    other_proxy.stop().await;
    other_platform.stop().await;
    drop(first);
    drop(other);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn unavailable_model_recovers_on_demand_without_replaying_the_http_turn() -> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::ActiveTransportFallback,
        delay_http: false,
    })
    .await?;
    let (proxy, metrics) = start_test_proxy(&server).await?;
    proxy.set_capability_for_test("gpt-5.3-codex", CapabilityTransport::WebSocket);
    let (mut client, _) = connect_local(&proxy).await?;
    send_create_with_metadata(&mut client).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    send_create_with_metadata(&mut client).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    *server.fixture.state.capability_websocket.lock().await = Some(false);

    // No wall-clock five-minute sleep: advancing the recovery clock alone sends no probe.
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(31)).await;
    tokio::time::resume();
    assert_eq!(
        server.fixture.state.counts.lock().await.capability_requests,
        0
    );
    let mut changed = proxy.capability_changes();
    send_create_with_metadata(&mut client).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    server.fixture.wait_capabilities(1).await?;
    tokio::time::timeout(Duration::from_secs(1), changed.changed())
        .await
        .map_err(io::Error::other)?
        .map_err(io::Error::other)?;
    assert_eq!(server.fixture.counts().await.private_messages, 2);
    assert_eq!(metrics.snapshot().hybrid_capability_http, 1);
    // A confirmed no-WS reply renews the short recovery budget, not a five-minute
    // detour through status polling. Requests within that budget do not probe.
    send_create_with_metadata(&mut client).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    assert_eq!(
        server.fixture.state.counts.lock().await.capability_requests,
        1
    );
    *server.fixture.state.capability_websocket.lock().await = Some(true);
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(31)).await;
    tokio::time::resume();
    let mut changed = proxy.capability_changes();
    send_create_with_metadata(&mut client).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    server.fixture.wait_capabilities(2).await?;
    tokio::time::timeout(Duration::from_secs(1), changed.changed())
        .await
        .map_err(io::Error::other)?
        .map_err(io::Error::other)?;
    assert_eq!(server.fixture.counts().await.private_messages, 2);
    assert_eq!(metrics.snapshot().hybrid_capability_http, 3);

    // Only a later independent turn may use the newly proven WS capability.
    send_create_with_metadata(&mut client).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    assert_eq!(server.fixture.counts().await.private_messages, 3);
    assert_eq!(
        server.fixture.state.counts.lock().await.capability_requests,
        2
    );
    assert_eq!(metrics.snapshot().hybrid_recovery_http, 1);
    assert_eq!(metrics.snapshot().hybrid_ws, 2);
    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn earlier_capability_query_cannot_revive_newer_unavailability_and_refreshes_coalesce()
-> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::ActiveTransportFallback,
        delay_http: false,
    })
    .await?;
    let (proxy, _) = start_test_proxy(&server).await?;
    proxy.set_capability_for_test("gpt-5.3-codex", CapabilityTransport::WebSocket);
    let (mut client, _) = connect_local(&proxy).await?;
    send_create_with_metadata(&mut client).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    *server.fixture.state.capability_websocket.lock().await = Some(true);
    server
        .fixture
        .state
        .hold_capability
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let models = ["gpt-5.3-codex".to_owned(), "missing-model".to_owned()];
    proxy.refresh_capabilities(&models);
    server.fixture.wait_capabilities(1).await?;
    for _ in 0..32 {
        proxy.refresh_capabilities(&models);
    }
    assert_eq!(
        server.fixture.state.counts.lock().await.capability_requests,
        1
    );
    send_create_with_metadata(&mut client).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");

    // The entire positive batch began BEFORE the negative fact; it must not
    // make its previously unknown models appear freshly confirmed either.
    *server.fixture.state.capability_websocket.lock().await = Some(false);
    server.fixture.state.release_capability.notify_one();
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            proxy.refresh_capabilities(&models);
            if server.fixture.state.counts.lock().await.capability_requests == 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .map_err(io::Error::other)?;
    assert_eq!(
        proxy
            .capability_statuses()
            .get("gpt-5.3-codex")
            .map(|status| status.transport),
        Some(CapabilityTransport::HttpOnly)
    );
    send_create_with_metadata(&mut client).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    assert_eq!(server.fixture.counts().await.private_messages, 2);
    for _ in 0..32 {
        proxy.refresh_capabilities(&models);
    }
    assert_eq!(
        server.fixture.state.counts.lock().await.capability_requests,
        2
    );
    let mut changed = proxy.capability_changes();
    server.fixture.state.release_capability.notify_one();
    tokio::time::timeout(Duration::from_secs(1), changed.changed())
        .await
        .map_err(io::Error::other)?
        .map_err(io::Error::other)?;
    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn capability_refresh_from_status_cannot_recover_a_negative_model() -> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::ActiveTransportFallback,
        delay_http: false,
    })
    .await?;
    let (proxy, _) = start_test_proxy(&server).await?;
    proxy.set_capability_for_test("gpt-5.3-codex", CapabilityTransport::WebSocket);
    proxy.set_capability_for_test("other-model", CapabilityTransport::WebSocket);
    let (mut client, _) = connect_local(&proxy).await?;
    send_create_with_metadata(&mut client).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    send_create_with_metadata(&mut client).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    *server.fixture.state.capability_websocket.lock().await = Some(true);
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(301)).await;
    tokio::time::resume();
    let mut changed = proxy.capability_changes();
    proxy.refresh_capabilities(&["other-model".to_owned()]);
    server.fixture.wait_capabilities(1).await?;
    tokio::time::timeout(Duration::from_secs(1), changed.changed())
        .await
        .map_err(io::Error::other)?
        .map_err(io::Error::other)?;
    let statuses = proxy.capability_statuses();
    assert_eq!(
        statuses.get("gpt-5.3-codex").map(|status| status.transport),
        Some(CapabilityTransport::HttpOnly)
    );
    assert_eq!(
        statuses.get("other-model").map(|status| status.transport),
        Some(CapabilityTransport::WebSocket)
    );
    let batches = server.fixture.state.capability_models.lock().await.clone();
    assert_eq!(batches.first(), Some(&vec!["other-model".to_owned()]));
    assert_eq!(server.fixture.counts().await.private_messages, 2);
    // An unknown-model status query must also omit A even if the caller supplies it.
    let mut changed = proxy.capability_changes();
    proxy.refresh_capabilities(&["unknown-model".to_owned(), "gpt-5.3-codex".to_owned()]);
    server.fixture.wait_capabilities(2).await?;
    tokio::time::timeout(Duration::from_secs(1), changed.changed())
        .await
        .map_err(io::Error::other)?
        .map_err(io::Error::other)?;
    let batches = server.fixture.state.capability_models.lock().await.clone();
    assert!(
        batches
            .iter()
            .all(|batch| !batch.iter().any(|model| model == "gpt-5.3-codex"))
    );
    assert_eq!(
        proxy
            .capability_statuses()
            .get("gpt-5.3-codex")
            .map(|status| status.transport),
        Some(CapabilityTransport::HttpOnly)
    );
    // A new independent business request may include A and prove recovery, but
    // its already selected HTTP request is never moved to WS.
    let mut changed = proxy.capability_changes();
    send_create_with_metadata(&mut client).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    server.fixture.wait_capabilities(3).await?;
    tokio::time::timeout(Duration::from_secs(1), changed.changed())
        .await
        .map_err(io::Error::other)?
        .map_err(io::Error::other)?;
    assert!(
        server
            .fixture
            .state
            .capability_models
            .lock()
            .await
            .last()
            .is_some_and(|batch| batch.iter().any(|model| model == "gpt-5.3-codex"))
    );
    assert_eq!(server.fixture.counts().await.private_messages, 2);
    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn capability_refresh_from_stopped_proxy_cannot_overwrite_new_proxy_negative_snapshot()
-> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::ActiveTransportFallback,
        delay_http: false,
    })
    .await?;
    let policy_path = server.directory.path().join("shared-policy.json");
    let (old_proxy, _) = start_test_proxy_with_policy(&server, Some(policy_path.clone())).await?;
    *server.fixture.state.capability_websocket.lock().await = Some(true);
    server
        .fixture
        .state
        .hold_capability
        .store(true, std::sync::atomic::Ordering::Relaxed);
    old_proxy.refresh_capabilities(&["gpt-5.3-codex".to_owned()]);
    server.fixture.wait_capabilities(1).await?;
    let mut old_changes = old_proxy.capability_changes();
    old_proxy.stop().await;

    let (proxy, _) = start_test_proxy_with_policy(&server, Some(policy_path.clone())).await?;
    proxy.set_capability_for_test("gpt-5.3-codex", CapabilityTransport::WebSocket);
    let (mut client, _) = connect_local(&proxy).await?;
    send_create_with_metadata(&mut client).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    send_create_with_metadata(&mut client).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    drop(client);
    proxy.stop().await;

    // The old GET is allowed to finish only after the newer negative snapshot is durable.
    server.fixture.state.release_capability.notify_one();
    tokio::time::timeout(Duration::from_secs(1), async {
        while old_changes.changed().await.is_ok() {}
    })
    .await
    .map_err(io::Error::other)?;
    let (restarted, _) = start_test_proxy_with_policy(&server, Some(policy_path)).await?;
    assert_eq!(
        restarted
            .capability_statuses()
            .get("gpt-5.3-codex")
            .map(|status| status.transport),
        Some(CapabilityTransport::HttpOnly)
    );
    let (mut client, _) = connect_local(&restarted).await?;
    send_create_with_metadata(&mut client).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    assert_eq!(server.fixture.counts().await.private_messages, 2);
    drop(client);
    restarted.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn capability_context_capacity_keeps_excess_credentials_http_without_probe_storm()
-> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::Persistent,
        delay_http: false,
    })
    .await?;
    *server.fixture.state.capability_websocket.lock().await = Some(true);
    let (proxy, _) = start_test_proxy(&server).await?;
    let mut clients = Vec::new();
    for index in 0..32 {
        let authorization = format!("Bearer fixture-context-{index}");
        let (mut client, _) =
            connect_local_with_authorization(&proxy, Some(&authorization)).await?;
        client
            .send(Message::Ping(b"ready".to_vec().into()))
            .await
            .map_err(io::Error::other)?;
        if !matches!(client.next().await, Some(Ok(Message::Pong(_)))) {
            return Err(io::Error::other("local context did not become ready"));
        }
        clients.push(client);
    }
    // The startup context plus 31 live credential contexts exhaust the retained bound.
    // The excess context still completes over HTTP and cannot start its own probe loop.
    let client = clients
        .last_mut()
        .ok_or_else(|| io::Error::other("context missing"))?;
    for _ in 0..3 {
        send_create_with_metadata(client).await?;
        assert_eq!(next_event_type(client).await?, "response.completed");
    }
    assert_eq!(server.fixture.counts().await.private_handshakes, 0);
    assert_eq!(server.fixture.counts().await.http_requests, 3);
    assert_eq!(
        server.fixture.state.counts.lock().await.capability_requests,
        0
    );
    drop(clients);
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
    proxy.set_capability_for_test("test", CapabilityTransport::WebSocket);
    let mut changed = proxy.capability_changes();
    let (mut client, _) = connect_local(&proxy).await?;
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
    assert_counts_with_min_private(server.fixture.counts().await, 1, 2, 0);
    // The contract is still capability evidence even when this turn cannot be replayed.
    assert_eq!(
        proxy
            .capability_statuses()
            .get("test")
            .map(|status| status.transport),
        Some(CapabilityTransport::HttpOnly)
    );
    tokio::time::timeout(Duration::from_secs(1), changed.changed())
        .await
        .map_err(io::Error::other)?
        .map_err(io::Error::other)?;
    send_create(&mut client).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    assert_eq!(server.fixture.counts().await.private_messages, 2);
    assert_eq!(server.fixture.counts().await.http_requests, 1);

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
    proxy.set_capability_for_test("test", CapabilityTransport::WebSocket);
    let (mut client, _) = connect_local(&proxy).await?;
    send_create(&mut client).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");

    // When: the next request fails without the dedicated machine-readable code.
    send_create(&mut client).await?;

    // Then: Turbo forwards the generic error and performs no HTTP replay.
    assert_eq!(next_event_type(&mut client).await?, "error");
    assert_counts_with_min_private(server.fixture.counts().await, 1, 2, 0);
    assert_eq!(
        proxy
            .capability_statuses()
            .get("test")
            .map(|status| status.transport),
        Some(CapabilityTransport::WebSocket)
    );

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
    proxy.set_capability_for_test("test", CapabilityTransport::WebSocket);
    let mut changed = proxy.capability_changes();
    let (mut client, _) = connect_local(&proxy).await?;
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
    assert_counts_with_min_private(server.fixture.counts().await, 1, 2, 0);
    // The contract is still capability evidence even when this turn cannot be replayed.
    assert_eq!(
        proxy
            .capability_statuses()
            .get("test")
            .map(|status| status.transport),
        Some(CapabilityTransport::HttpOnly)
    );
    tokio::time::timeout(Duration::from_secs(1), changed.changed())
        .await
        .map_err(io::Error::other)?
        .map_err(io::Error::other)?;
    send_create(&mut client).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    assert_eq!(server.fixture.counts().await.private_messages, 2);
    assert_eq!(server.fixture.counts().await.http_requests, 1);

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
    proxy.set_capability_for_test("test", CapabilityTransport::WebSocket);
    let mut changed = proxy.capability_changes();
    let (mut client, _) = connect_local(&proxy).await?;
    send_create(&mut client).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    send_create(&mut client).await?;
    server.fixture.wait_messages(2).await?;

    // When: cancellation wins before New API emits the fallback contract.
    send_cancel(&mut client).await?;
    server.fixture.wait_messages(3).await?;

    // Then: Turbo forwards the terminal error without restarting the cancelled request.
    assert_eq!(next_event_type(&mut client).await?, "error");
    assert_counts_with_min_private(server.fixture.counts().await, 1, 3, 0);
    // The contract is still capability evidence even when this turn cannot be replayed.
    assert_eq!(
        proxy
            .capability_statuses()
            .get("test")
            .map(|status| status.transport),
        Some(CapabilityTransport::HttpOnly)
    );
    tokio::time::timeout(Duration::from_secs(1), changed.changed())
        .await
        .map_err(io::Error::other)?
        .map_err(io::Error::other)?;
    send_create(&mut client).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    assert_eq!(server.fixture.counts().await.private_messages, 3);
    assert_eq!(server.fixture.counts().await.http_requests, 1);

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
    proxy.set_capability_for_test("test", CapabilityTransport::WebSocket);
    let mut changed = proxy.capability_changes();
    let (mut client, _) = connect_local(&proxy).await?;
    send_create(&mut client).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");

    // When: cancellation races the already-queued fallback contract.
    send_create(&mut client).await?;
    send_cancel(&mut client).await?;

    // Then: Turbo treats the cancellation as a hard no-replay boundary.
    assert_eq!(next_event_type(&mut client).await?, "error");
    assert_counts_with_min_private(server.fixture.counts().await, 1, 2, 0);
    // The contract is still capability evidence even when this turn cannot be replayed.
    assert_eq!(
        proxy
            .capability_statuses()
            .get("test")
            .map(|status| status.transport),
        Some(CapabilityTransport::HttpOnly)
    );
    tokio::time::timeout(Duration::from_secs(1), changed.changed())
        .await
        .map_err(io::Error::other)?
        .map_err(io::Error::other)?;
    send_create(&mut client).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    assert_eq!(server.fixture.counts().await.private_messages, 2);
    assert_eq!(server.fixture.counts().await.http_requests, 1);

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
    send_create(&mut client).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");

    // When: the single HTTP fallback returns 413.
    send_create(&mut client).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.failed");

    // Then: the HTTP attempt is terminal for transport recovery and the local WS stays open.
    assert_counts_with_min_private(server.fixture.counts().await, 1, 2, 1);
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

#[tokio::test]
async fn cancelling_local_warmup_keeps_client_session_usable() -> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::Persistent,
        delay_http: false,
    })
    .await?;
    let (proxy, _) = start_test_proxy(&server).await?;
    proxy.set_capability_for_test("test", CapabilityTransport::HttpOnly);
    let (mut client, _) = connect_local(&proxy).await?;
    client
        .feed(Message::Text(
            serde_json::json!({
                "type":"response.create", "model":"test", "input":[], "generate":false,
            })
            .to_string()
            .into(),
        ))
        .await
        .map_err(io::Error::other)?;
    client
        .feed(Message::Text(
            serde_json::json!({"type":"response.cancel"})
                .to_string()
                .into(),
        ))
        .await
        .map_err(io::Error::other)?;
    client.flush().await.map_err(io::Error::other)?;
    loop {
        let event = next_event_type(&mut client).await?;
        if event == "response.completed" || event == "response.cancelled" {
            break;
        }
        assert_eq!(event, "response.created");
    }
    assert_counts(server.fixture.counts().await, 0, 0, 0);
    send_create(&mut client).await?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    assert_counts(server.fixture.counts().await, 0, 0, 1);
    drop(client);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}

#[tokio::test]
async fn capability_shutdown_reclaims_parked_handoff_before_expiry() -> io::Result<()> {
    let server = FixtureServer::start(FixtureConfig {
        private: PrivateBehavior::Persistent,
        delay_http: false,
    })
    .await?;
    let (proxy, _) = start_test_proxy(&server).await?;
    proxy.set_capability_for_test("test", CapabilityTransport::WebSocket);
    let (mut client, _) =
        connect_local_with_headers(&proxy, None, Some("parked-thread"), None).await?;
    client
        .send(Message::Text(
            serde_json::json!({
                "type":"response.create", "model":"test", "input":"test",
                "client_metadata":{"thread_id":"parked-thread"},
            })
            .to_string()
            .into(),
        ))
        .await
        .map_err(io::Error::other)?;
    assert_eq!(next_event_type(&mut client).await?, "response.completed");
    client.close(None).await.map_err(io::Error::other)?;
    assert!(
        tokio::time::timeout(
            Duration::from_millis(30),
            server.fixture.wait_normal_closes(1)
        )
        .await
        .is_err()
    );
    proxy.set_capability_for_test("test", CapabilityTransport::HttpOnly);
    tokio::time::timeout(
        Duration::from_millis(200),
        server.fixture.wait_normal_closes(1),
    )
    .await
    .map_err(io::Error::other)??;
    assert_eq!(proxy.connection_snapshot().await.current_connections, 0);
    assert_counts(server.fixture.counts().await, 1, 1, 0);
    proxy.stop().await;
    server.stop().await;
    Ok(())
}
