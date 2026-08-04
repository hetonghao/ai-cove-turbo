use std::{
    env, io,
    sync::{Arc, atomic::AtomicBool},
    time::Instant,
};

use futures_util::{SinkExt, StreamExt};
use reqwest::header::{AUTHORIZATION, HeaderValue};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    time::timeout,
};
use tokio_tungstenite::{
    WebSocketStream, connect_async,
    tungstenite::{client::IntoClientRequest, protocol::Message},
};
use url::Url;

use super::{
    ACK_TIMEOUT, PRODUCTION_ACK_UPSTREAM, TransportAck, ack_url, fixture_payload,
    observe::AckObservation, parse_ack, validate_ack, validate_metric_correlation,
};
use crate::proxy::{Metrics, ProxyOptions, start_proxy};

mod report;

const SAMPLE_COUNT: usize = 12;

struct SessionSample {
    index: usize,
    phase: &'static str,
    setup_ms: f64,
    ack_rtt_ms: f64,
    ack: TransportAck,
}

struct MeasurementContext<'a> {
    websocket_url: &'a str,
    authorization: &'a str,
    payload: &'a str,
    metrics: &'a Metrics,
}

async fn read_websocket_ack<S>(socket: &mut WebSocketStream<S>) -> Result<TransportAck, io::Error>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    loop {
        let message = timeout(ACK_TIMEOUT, socket.next())
            .await
            .map_err(io::Error::other)?
            .ok_or_else(|| io::Error::other("WebSocket transport ACK ended without a response"))?;
        match message.map_err(io::Error::other)? {
            Message::Text(payload) => return parse_ack(payload.as_ref()),
            Message::Binary(_) => return Err(io::Error::other("ACK response must be text")),
            Message::Ping(payload) => {
                timeout(ACK_TIMEOUT, socket.send(Message::Pong(payload)))
                    .await
                    .map_err(io::Error::other)?
                    .map_err(io::Error::other)?;
            }
            Message::Pong(_) => {}
            Message::Close(_) => {
                return Err(io::Error::other("ACK closed without a response"));
            }
            Message::Frame(_) => return Err(io::Error::other("ACK returned a raw frame")),
        }
    }
}

async fn send_websocket(
    url: &str,
    authorization: &str,
    payload: &str,
) -> Result<AckObservation, io::Error> {
    let setup_started = Instant::now();
    let mut request = url.into_client_request().map_err(io::Error::other)?;
    request.headers_mut().insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {authorization}")).map_err(io::Error::other)?,
    );
    let (mut socket, response) = timeout(ACK_TIMEOUT, connect_async(request))
        .await
        .map_err(io::Error::other)?
        .map_err(io::Error::other)?;
    if response.status().as_u16() != 101 {
        return Err(io::Error::other(format!(
            "WebSocket transport ACK status {}",
            response.status()
        )));
    }
    let setup_ms = setup_started.elapsed().as_secs_f64() * 1_000.0;
    let ack_started = Instant::now();
    timeout(
        ACK_TIMEOUT,
        socket.send(Message::Text(payload.to_owned().into())),
    )
    .await
    .map_err(io::Error::other)?
    .map_err(io::Error::other)?;
    let ack = read_websocket_ack(&mut socket).await?;
    let ack_rtt_ms = ack_started.elapsed().as_secs_f64() * 1_000.0;
    timeout(ACK_TIMEOUT, socket.close(None))
        .await
        .map_err(io::Error::other)?
        .map_err(io::Error::other)?;
    Ok(AckObservation::websocket(ack, ack_rtt_ms, setup_ms))
}

async fn measure_sequential(
    context: MeasurementContext<'_>,
) -> Result<Vec<SessionSample>, io::Error> {
    let expected_decoded_bytes = u64::try_from(context.payload.len()).map_err(io::Error::other)?;
    let mut samples = Vec::with_capacity(SAMPLE_COUNT);
    for index in 0..SAMPLE_COUNT {
        let before = context.metrics.snapshot();
        let connection = send_websocket(
            context.websocket_url,
            context.authorization,
            context.payload,
        )
        .await?;
        let ack = connection.ack;
        let setup_ms = connection
            .websocket_setup_ms
            .ok_or_else(|| io::Error::other("missing WebSocket setup timing"))?;
        let after = context.metrics.snapshot();
        if after
            .websocket_handshakes
            .saturating_sub(before.websocket_handshakes)
            != 1
        {
            return Err(io::Error::other(
                "Turbo WebSocket ACK did not use one handshake",
            ));
        }
        validate_ack(&ack, "websocket", expected_decoded_bytes)?;
        validate_metric_correlation(
            &ack,
            after
                .websocket_raw_bytes
                .saturating_sub(before.websocket_raw_bytes),
            after
                .websocket_sent_bytes
                .saturating_sub(before.websocket_sent_bytes),
        )?;
        if ack.wire_bytes >= ack.decoded_bytes {
            return Err(io::Error::other(
                "Turbo WebSocket ACK did not reduce the long fixture",
            ));
        }
        samples.push(SessionSample {
            index: index + 1,
            phase: if index == 0 { "cold" } else { "warm" },
            setup_ms,
            ack_rtt_ms: connection.client_ack_rtt_ms,
            ack,
        });
    }
    Ok(samples)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires explicit production ACK authorization"]
async fn production_ack_tls_session_reuse_has_cold_and_warm_samples() -> Result<(), io::Error> {
    let upstream = env::var("TURBO_TRANSPORT_ACK_UPSTREAM").map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "TURBO_TRANSPORT_ACK_UPSTREAM is required",
        )
    })?;
    let authorization = env::var("AI_COVE_API_KEY")
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "AI_COVE_API_KEY is required"))?;
    if authorization.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "AI_COVE_API_KEY is empty",
        ));
    }
    let allow_production = env::var("TURBO_TRANSPORT_ACK_ALLOW_PRODUCTION").as_deref() == Ok("1");
    let upstream_url = Url::parse(&upstream).map_err(io::Error::other)?;
    let _ = ack_url(&upstream, true, allow_production)?;
    if upstream != PRODUCTION_ACK_UPSTREAM {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Issue #28 production measurement requires the approved production ACK upstream",
        ));
    }
    let metrics = Arc::new(Metrics::default());
    let proxy = start_proxy(ProxyOptions {
        upstream: upstream_url,
        compression_enabled: Arc::new(AtomicBool::new(true)),
        websocket_enabled: Arc::new(AtomicBool::new(true)),
        ai_cove_private_websocket_zstd: true,
        metrics: Arc::clone(&metrics),
        preferred_ports: vec![0],
        max_request_body_bytes: 1 << 20,
    })
    .await
    .map_err(io::Error::other)?;
    let websocket_url = ack_url(proxy.endpoint(), true, false)?;
    let payload = fixture_payload();
    let result = measure_sequential(MeasurementContext {
        websocket_url: &websocket_url,
        authorization: &authorization,
        payload: &payload,
        metrics: metrics.as_ref(),
    })
    .await;
    proxy.stop().await;
    let samples = result?;
    report::print_report(&samples)?;
    Ok(())
}
