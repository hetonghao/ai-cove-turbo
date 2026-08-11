use std::{
    env,
    error::Error,
    io,
    sync::{Arc, atomic::AtomicBool},
    time::Instant,
};

use futures_util::{SinkExt, StreamExt};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderValue};
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
    ACK_TIMEOUT, TransportAck, ack_url, fixture_payload,
    observe::{AckObservation, print_observation},
    parse_ack, validate_ack, validate_identity_ack, validate_metric_correlation,
};
#[cfg(test)]
use crate::proxy::private_websocket_benchmark::start as start_private_websocket_proxy;
use crate::proxy::{Metrics, ProxyOptions, start_proxy};

type AckResult<T> = Result<T, io::Error>;

fn ack_error(error: impl Error + Send + Sync + 'static) -> io::Error {
    io::Error::other(error)
}

struct LiveSetup {
    client: reqwest::Client,
    authorization: String,
    payload: String,
    direct_http_url: String,
    turbo_http_url: String,
    direct_ws_url: String,
    turbo_ws_url: String,
    http_metrics: Arc<Metrics>,
    websocket_metrics: Arc<Metrics>,
}

async fn send_http(
    client: &reqwest::Client,
    url: &str,
    authorization: &str,
    payload: &str,
) -> AckResult<AckObservation> {
    let started = Instant::now();
    let response = timeout(
        ACK_TIMEOUT,
        client
            .post(url)
            .header(AUTHORIZATION, format!("Bearer {authorization}"))
            .header(CONTENT_TYPE, "application/json")
            .body(payload.to_owned())
            .send(),
    )
    .await
    .map_err(ack_error)?
    .map_err(ack_error)?;
    let status = response.status();
    let body = timeout(ACK_TIMEOUT, response.bytes())
        .await
        .map_err(ack_error)?
        .map_err(ack_error)?;
    if !status.is_success() {
        return Err(io::Error::other(format!(
            "HTTP transport ACK status {status}"
        )));
    }
    Ok(AckObservation::http(
        parse_ack(&body)?,
        started.elapsed().as_secs_f64() * 1_000.0,
    ))
}

async fn read_websocket_ack<S>(socket: &mut WebSocketStream<S>) -> AckResult<TransportAck>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    loop {
        let message = timeout(ACK_TIMEOUT, socket.next())
            .await
            .map_err(ack_error)?
            .ok_or_else(|| io::Error::other("WebSocket transport ACK ended without a response"))?;
        match message.map_err(ack_error)? {
            Message::Text(payload) => return parse_ack(payload.as_ref()),
            Message::Binary(_) => return Err(io::Error::other("ACK response must be text")),
            Message::Ping(payload) => {
                timeout(ACK_TIMEOUT, socket.send(Message::Pong(payload)))
                    .await
                    .map_err(ack_error)?
                    .map_err(ack_error)?;
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
) -> AckResult<AckObservation> {
    let setup_started = Instant::now();
    let mut request = url.into_client_request().map_err(ack_error)?;
    request.headers_mut().insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {authorization}")).map_err(ack_error)?,
    );
    let (mut socket, response) = timeout(ACK_TIMEOUT, connect_async(request))
        .await
        .map_err(ack_error)?
        .map_err(ack_error)?;
    if response.status().as_u16() != 101 {
        return Err(io::Error::other(format!(
            "WebSocket transport ACK status {}",
            response.status()
        )));
    }
    let websocket_setup_ms = setup_started.elapsed().as_secs_f64() * 1_000.0;
    let ack_started = Instant::now();
    timeout(
        ACK_TIMEOUT,
        socket.send(Message::Text(payload.to_owned().into())),
    )
    .await
    .map_err(ack_error)?
    .map_err(ack_error)?;
    let ack = read_websocket_ack(&mut socket).await?;
    let client_ack_rtt_ms = ack_started.elapsed().as_secs_f64() * 1_000.0;
    timeout(ACK_TIMEOUT, socket.close(None))
        .await
        .map_err(ack_error)?
        .map_err(ack_error)?;
    Ok(AckObservation::websocket(
        ack,
        client_ack_rtt_ms,
        websocket_setup_ms,
    ))
}

async fn exercise_paths(setup: &LiveSetup) -> AckResult<()> {
    let payload_bytes = u64::try_from(setup.payload.len()).map_err(ack_error)?;

    let direct_http = send_http(
        &setup.client,
        &setup.direct_http_url,
        &setup.authorization,
        &setup.payload,
    )
    .await?;
    validate_identity_ack(&direct_http.ack, "http", payload_bytes)?;
    print_observation("direct_http", &direct_http);

    let before_http = setup.http_metrics.snapshot();
    let turbo_http = send_http(
        &setup.client,
        &setup.turbo_http_url,
        &setup.authorization,
        &setup.payload,
    )
    .await?;
    let after_http = setup.http_metrics.snapshot();
    if after_http.requests.saturating_sub(before_http.requests) != 1 {
        return Err(io::Error::other(
            "Turbo HTTP ACK did not record one request",
        ));
    }
    validate_ack(&turbo_http.ack, "http", payload_bytes)?;
    validate_metric_correlation(
        &turbo_http.ack,
        after_http.raw_bytes.saturating_sub(before_http.raw_bytes),
        after_http.sent_bytes.saturating_sub(before_http.sent_bytes),
    )?;
    if turbo_http.ack.wire_bytes >= turbo_http.ack.decoded_bytes {
        return Err(io::Error::other(
            "Turbo HTTP ACK did not reduce the long fixture",
        ));
    }
    print_observation("turbo_http", &turbo_http);

    let direct_websocket =
        send_websocket(&setup.direct_ws_url, &setup.authorization, &setup.payload).await?;
    validate_identity_ack(&direct_websocket.ack, "websocket", payload_bytes)?;
    print_observation("direct_websocket", &direct_websocket);

    let before_websocket = setup.websocket_metrics.snapshot();
    let turbo_websocket =
        send_websocket(&setup.turbo_ws_url, &setup.authorization, &setup.payload).await?;
    let after_websocket = setup.websocket_metrics.snapshot();
    if after_websocket
        .websocket_handshakes
        .saturating_sub(before_websocket.websocket_handshakes)
        != 1
    {
        return Err(io::Error::other(
            "Turbo WebSocket ACK did not use one handshake",
        ));
    }
    validate_ack(&turbo_websocket.ack, "websocket", payload_bytes)?;
    validate_metric_correlation(
        &turbo_websocket.ack,
        after_websocket
            .websocket_raw_bytes
            .saturating_sub(before_websocket.websocket_raw_bytes),
        after_websocket
            .websocket_sent_bytes
            .saturating_sub(before_websocket.websocket_sent_bytes),
    )?;
    if turbo_websocket.ack.wire_bytes >= turbo_websocket.ack.decoded_bytes {
        return Err(io::Error::other(
            "Turbo WebSocket ACK did not reduce the long fixture",
        ));
    }
    print_observation("turbo_websocket", &turbo_websocket);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires explicit loopback or production ACK authorization"]
async fn live_transport_ack_benchmark() -> AckResult<()> {
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
    let upstream_url = Url::parse(&upstream).map_err(ack_error)?;
    let direct_http_url = ack_url(&upstream, false, allow_production)?;
    let direct_ws_url = ack_url(&upstream, true, allow_production)?;
    let client = reqwest::Client::builder()
        .timeout(ACK_TIMEOUT)
        .build()
        .map_err(ack_error)?;
    let payload = fixture_payload();
    let http_metrics = Arc::new(Metrics::default());
    let http_proxy = start_proxy(ProxyOptions {
        upstream: upstream_url.clone(),
        compression_enabled: Arc::new(AtomicBool::new(true)),
        websocket_enabled: Arc::new(AtomicBool::new(false)),
        ai_cove_private_websocket_zstd: false,
        metrics: Arc::clone(&http_metrics),
        preferred_ports: vec![0],
        max_request_body_bytes: 1 << 20,
    })
    .await
    .map_err(ack_error)?;
    let websocket_metrics = Arc::new(Metrics::default());
    let websocket_proxy =
        match start_private_websocket_proxy(upstream_url, Arc::clone(&websocket_metrics)).await {
            Ok(proxy) => proxy,
            Err(error) => {
                http_proxy.stop().await;
                return Err(ack_error(error));
            }
        };
    let setup = LiveSetup {
        client,
        authorization,
        payload,
        direct_http_url,
        turbo_http_url: ack_url(http_proxy.endpoint(), false, false)?,
        direct_ws_url,
        turbo_ws_url: ack_url(websocket_proxy.endpoint(), true, false)?,
        http_metrics,
        websocket_metrics,
    };
    let result = exercise_paths(&setup).await;
    http_proxy.stop().await;
    websocket_proxy.stop().await;
    result
}
