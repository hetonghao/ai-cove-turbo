use std::{
    error::Error,
    io,
    time::{Duration, Instant},
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

use super::super::{BenchmarkCase, BenchmarkSettings, Sample, metric_delta, response_is_complete};

async fn wait_for_response_complete<S>(
    socket: &mut WebSocketStream<S>,
) -> Result<(), Box<dyn Error>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    while let Some(message) = socket.next().await {
        match message? {
            Message::Text(text) if response_is_complete(text.as_ref()) => return Ok(()),
            Message::Binary(bytes)
                if std::str::from_utf8(&bytes).is_ok_and(response_is_complete) =>
            {
                return Ok(());
            }
            Message::Close(_) => {
                return Err(io::Error::other("WebSocket closed before response completion").into());
            }
            Message::Ping(_)
            | Message::Pong(_)
            | Message::Text(_)
            | Message::Binary(_)
            | Message::Frame(_) => {}
        }
    }
    Err(io::Error::other("WebSocket ended before response completion").into())
}

async fn sample(
    url: &str,
    authorization: &str,
    payload: &str,
    timeout_duration: Duration,
) -> Result<Sample, Box<dyn Error>> {
    let started = Instant::now();
    let mut request = url.into_client_request()?;
    request.headers_mut().insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {authorization}"))?,
    );
    let (mut socket, response) = timeout(timeout_duration, connect_async(request)).await??;
    if response.status().as_u16() != 101 {
        return Err(io::Error::other(format!(
            "WebSocket benchmark response status {}",
            response.status()
        ))
        .into());
    }
    let setup = started.elapsed();
    let send_started = Instant::now();
    timeout(
        timeout_duration,
        socket.send(Message::Text(payload.to_owned().into())),
    )
    .await??;
    let transport = send_started.elapsed();
    timeout(timeout_duration, wait_for_response_complete(&mut socket)).await??;
    Ok(Sample {
        e2e: started.elapsed(),
        transport,
        setup,
        raw_bytes: u64::try_from(payload.len())?,
        wire_bytes: 0,
    })
}

pub(super) async fn collect_case(
    url: &str,
    authorization: &str,
    payload: &str,
    settings: &BenchmarkSettings,
    metrics: &crate::proxy::Metrics,
) -> Result<BenchmarkCase, Box<dyn Error>> {
    let mut samples = Vec::with_capacity(settings.runs);
    for iteration in 0..settings.warmups + settings.runs {
        let before = metrics.snapshot();
        let mut sample = sample(url, authorization, payload, settings.timeout).await?;
        let after = metrics.snapshot();
        let (raw_bytes, wire_bytes) = metric_delta(before, after, true);
        if raw_bytes == 0 || wire_bytes == 0 {
            return Err(io::Error::other("Turbo did not record the WebSocket zstd request").into());
        }
        sample.raw_bytes = raw_bytes;
        sample.wire_bytes = wire_bytes;
        if iteration >= settings.warmups {
            samples.push(sample);
        }
    }
    Ok(BenchmarkCase {
        name: "WS + zstd",
        samples,
    })
}
