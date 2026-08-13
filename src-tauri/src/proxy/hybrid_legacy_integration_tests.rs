use std::io;

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

use super::{connect_local, next_event_type};

#[path = "hybrid_legacy_fixture.rs"]
mod fixture;

use fixture::{StandardFixture, start_standard_proxy, wait_for_traffic};

const CREATE: &str = r#"{"type":"response.create","model":"test","input":[]}"#;
const FAIL_CREATE: &str = r#"{"type":"response.create","model":"close-active","input":[]}"#;
const FAILED_TERMINAL_CREATE: &str =
    r#"{"type":"response.create","model":"failed-terminal","input":[]}"#;

#[tokio::test]
async fn standard_legacy_relay_records_each_terminal_once() -> io::Result<()> {
    // Given: private prewarm is unavailable and a compatibility frame selects standard WS relay.
    let fixture = StandardFixture::start().await?;
    let (proxy, metrics) = start_standard_proxy(&fixture).await?;
    let (mut client, status) = connect_local(&proxy).await?;
    assert_eq!(status, 101);
    client
        .send(Message::Text(r#"{"type":"session.update"}"#.into()))
        .await
        .map_err(io::Error::other)?;

    // When: two sequential response.create requests each reach a terminal event.
    for _ in 0..2 {
        client
            .send(Message::Text(CREATE.into()))
            .await
            .map_err(io::Error::other)?;
        assert_eq!(next_event_type(&mut client).await?, "response.completed");
        wait_for_traffic(&metrics).await?;
    }

    // Then: initialization is excluded and each request has one truthful standard WS outcome.
    let events = serde_json::to_value(metrics.traffic_snapshot().recent_requests)
        .map_err(io::Error::other)?;
    let events = events
        .as_array()
        .ok_or_else(|| io::Error::other("traffic events are not an array"))?;
    assert_eq!(events.len(), 2);
    for event in events {
        assert_eq!(event.get("route"), Some(&serde_json::json!("hybridWs")));
        assert_eq!(event.get("result"), Some(&serde_json::json!("success")));
        assert_eq!(
            event.get("rawBytes"),
            Some(&serde_json::json!(CREATE.len()))
        );
        assert_eq!(
            event.get("sentBytes"),
            Some(&serde_json::json!(CREATE.len()))
        );
    }

    drop(client);
    proxy.stop().await;
    fixture.stop().await;
    Ok(())
}

#[tokio::test]
async fn standard_legacy_relay_records_active_close_as_one_error() -> io::Result<()> {
    // Given: standard relay has accepted one response.create after its compatibility frame.
    let fixture = StandardFixture::start().await?;
    let (proxy, metrics) = start_standard_proxy(&fixture).await?;
    let (mut client, status) = connect_local(&proxy).await?;
    assert_eq!(status, 101);
    client
        .send(Message::Text(r#"{"type":"session.update"}"#.into()))
        .await
        .map_err(io::Error::other)?;

    // When: upstream closes the active request before sending a terminal event.
    client
        .send(Message::Text(FAIL_CREATE.into()))
        .await
        .map_err(io::Error::other)?;
    let Some(Ok(Message::Close(_))) = client.next().await else {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "standard relay did not forward active close",
        ));
    };
    wait_for_traffic(&metrics).await?;

    // Then: the submitted request has one error outcome and no early success record.
    let events = serde_json::to_value(metrics.traffic_snapshot().recent_requests)
        .map_err(io::Error::other)?;
    let events = events
        .as_array()
        .ok_or_else(|| io::Error::other("traffic events are not an array"))?;
    assert_eq!(events.len(), 1);
    let event = events
        .first()
        .ok_or_else(|| io::Error::other("active close outcome missing"))?;
    assert_eq!(event.get("route"), Some(&serde_json::json!("hybridWs")));
    assert_eq!(event.get("result"), Some(&serde_json::json!("error")));
    assert_eq!(
        event.get("failurePhase"),
        Some(&serde_json::json!("hybridActive"))
    );
    assert_eq!(
        event.get("rawBytes"),
        Some(&serde_json::json!(FAIL_CREATE.len()))
    );
    assert_eq!(
        event.get("sentBytes"),
        Some(&serde_json::json!(FAIL_CREATE.len()))
    );

    proxy.stop().await;
    fixture.stop().await;
    Ok(())
}

#[tokio::test]
async fn standard_legacy_relay_records_failed_terminal_as_one_error() -> io::Result<()> {
    // Given: standard relay has accepted one response.create after its compatibility frame.
    let fixture = StandardFixture::start().await?;
    let (proxy, metrics) = start_standard_proxy(&fixture).await?;
    let (mut client, status) = connect_local(&proxy).await?;
    assert_eq!(status, 101);
    client
        .send(Message::Text(r#"{"type":"session.update"}"#.into()))
        .await
        .map_err(io::Error::other)?;

    // When: upstream returns an explicit failed terminal event without closing the connection.
    client
        .send(Message::Text(FAILED_TERMINAL_CREATE.into()))
        .await
        .map_err(io::Error::other)?;
    assert_eq!(next_event_type(&mut client).await?, "response.failed");
    wait_for_traffic(&metrics).await?;

    // Then: the submitted request has exactly one final error outcome.
    let events = serde_json::to_value(metrics.traffic_snapshot().recent_requests)
        .map_err(io::Error::other)?;
    let events = events
        .as_array()
        .ok_or_else(|| io::Error::other("traffic events are not an array"))?;
    assert_eq!(events.len(), 1);
    let event = events
        .first()
        .ok_or_else(|| io::Error::other("failed terminal outcome missing"))?;
    assert_eq!(event.get("route"), Some(&serde_json::json!("hybridWs")));
    assert_eq!(event.get("result"), Some(&serde_json::json!("error")));
    assert_eq!(
        event.get("failurePhase"),
        Some(&serde_json::json!("hybridActive"))
    );

    drop(client);
    proxy.stop().await;
    fixture.stop().await;
    Ok(())
}
