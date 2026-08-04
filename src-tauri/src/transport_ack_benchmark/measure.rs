use std::{
    error::Error,
    time::{Duration, Instant},
};

use bytes::Bytes;

use crate::proxy::{MIN_COMPRESSION_INPUT_BYTES, measure_http_encoding, measure_private_encoding};

const SAMPLE_COUNT: usize = 17;
const WARMUP_COUNT: usize = 3;
const SIZES: [usize; 13] = [
    64, 128, 256, 512, 1024, 2048, 4096, 8192, 16384, 32768, 65536, 98304, 131_072,
];
const SOURCE_PATTERN: &str = "transport request repeat 0123456789abcdef ";

fn source_like_payload(target_bytes: usize) -> Vec<u8> {
    let prefix = r#"{"model":"gpt-5.6-luna","input":""#;
    let suffix = r#""}"#;
    let input_bytes = target_bytes.saturating_sub(prefix.len() + suffix.len());
    let repeats = input_bytes.div_ceil(SOURCE_PATTERN.len());
    let mut input = SOURCE_PATTERN.repeat(repeats);
    input.truncate(input_bytes);
    let payload = format!("{prefix}{input}{suffix}");
    assert_eq!(payload.len(), target_bytes, "fixture must be exact-sized");
    payload.into_bytes()
}

fn milliseconds(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

fn format_duration_summary(durations: &[Duration]) -> Result<String, &'static str> {
    let minimum = durations.first().copied().ok_or("missing minimum sample")?;
    let median = durations
        .get(durations.len() / 2)
        .copied()
        .ok_or("missing median sample")?;
    let maximum = durations.last().copied().ok_or("missing maximum sample")?;
    Ok(format!(
        "{:.3}[{:.3},{:.3}]",
        milliseconds(median),
        milliseconds(minimum),
        milliseconds(maximum),
    ))
}

async fn http_sample(payload: &[u8]) -> Result<(usize, Duration), Box<dyn Error>> {
    let started = Instant::now();
    let encoded = measure_http_encoding(Bytes::copy_from_slice(payload))
        .await
        .map_err(|()| "HTTP zstd worker failed")?;
    let elapsed = started.elapsed();
    Ok((encoded.as_ref().map_or(payload.len(), Bytes::len), elapsed))
}

async fn websocket_sample(payload: &[u8]) -> Result<(usize, Duration), Box<dyn Error>> {
    let started = Instant::now();
    let encoded = measure_private_encoding(payload.to_vec(), false).await?;
    Ok((encoded.len(), started.elapsed()))
}

async fn measure_path(
    payload: &[u8],
    websocket: bool,
) -> Result<(usize, Vec<Duration>), Box<dyn Error>> {
    for _ in 0..WARMUP_COUNT {
        if websocket {
            websocket_sample(payload).await?;
        } else {
            http_sample(payload).await?;
        }
    }
    let mut wires = Vec::with_capacity(SAMPLE_COUNT);
    let mut durations = Vec::with_capacity(SAMPLE_COUNT);
    for _ in 0..SAMPLE_COUNT {
        let (wire, elapsed) = if websocket {
            websocket_sample(payload).await?
        } else {
            http_sample(payload).await?
        };
        wires.push(wire);
        durations.push(elapsed);
    }
    wires.sort_unstable();
    durations.sort_unstable();
    let wire = wires
        .get(SAMPLE_COUNT / 2)
        .copied()
        .ok_or("missing median wire sample")?;
    Ok((wire, durations))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "explicit local measurement; prints current-policy timing"]
async fn measure_current_encoding_paths() -> Result<(), Box<dyn Error>> {
    println!(
        "method=source-like-json; samples={SAMPLE_COUNT}; warmups={WARMUP_COUNT}; link_mbps=10"
    );
    for target in SIZES {
        let payload = source_like_payload(target);
        let raw = i32::try_from(payload.len())?;
        for websocket in [false, true] {
            let (wire, durations) = measure_path(&payload, websocket).await?;
            let wire = i32::try_from(wire)?;
            let saved = raw - wire;
            let saved_ms = f64::from(saved) * 8.0 / 10_000.0;
            let label = if websocket { "ws" } else { "http" };
            let duration_summary = format_duration_summary(&durations)?;
            let raw_samples = durations
                .iter()
                .map(|duration| format!("{:.3}", milliseconds(*duration)))
                .collect::<Vec<_>>()
                .join(",");
            println!(
                "path={label} target={target} raw={} wire={wire} saved={saved} saved_serialization_ms={saved_ms:.3} duration_ms={duration_summary} raw_samples_ms=[{raw_samples}]",
                payload.len(),
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn below_threshold_keeps_http_raw_and_private_ws_envelope_uncompressed()
-> Result<(), Box<dyn Error>> {
    let payload = source_like_payload(MIN_COMPRESSION_INPUT_BYTES - 1);

    let (http_wire, _) = http_sample(&payload).await?;
    assert_eq!(http_wire, payload.len());

    let websocket_wire = measure_private_encoding(payload.clone(), false).await?;
    assert_eq!(websocket_wire.len(), payload.len() + 10);
    assert_eq!(
        websocket_wire.get(5).copied().map(|flags| flags & 1),
        Some(0)
    );
    Ok(())
}

#[tokio::test]
async fn threshold_boundary_compresses_when_level_three_is_smaller() -> Result<(), Box<dyn Error>> {
    let payload = source_like_payload(MIN_COMPRESSION_INPUT_BYTES);

    let (http_wire, _) = http_sample(&payload).await?;
    assert!(http_wire < payload.len());

    let websocket_wire = measure_private_encoding(payload.clone(), false).await?;
    assert!(websocket_wire.len() < payload.len() + 10);
    assert_eq!(
        websocket_wire.get(5).copied().map(|flags| flags & 1),
        Some(1)
    );
    Ok(())
}

#[tokio::test]
async fn above_threshold_compresses_when_level_three_is_smaller() -> Result<(), Box<dyn Error>> {
    let payload = source_like_payload(MIN_COMPRESSION_INPUT_BYTES + 1);

    let (http_wire, _) = http_sample(&payload).await?;
    assert!(http_wire < payload.len());

    let websocket_wire = measure_private_encoding(payload.clone(), false).await?;
    assert!(websocket_wire.len() < payload.len() + 10);
    assert_eq!(
        websocket_wire.get(5).copied().map(|flags| flags & 1),
        Some(1)
    );
    Ok(())
}

#[test]
fn measured_threshold_and_fixture_lengths_are_explicit() {
    assert_eq!(MIN_COMPRESSION_INPUT_BYTES, 1024);
    for target in [128, 256, 1023, 1024, 1025] {
        assert_eq!(source_like_payload(target).len(), target);
    }
}

#[test]
fn formats_duration_summary_as_median_then_range() {
    let samples = [
        Duration::from_micros(125),
        Duration::from_micros(250),
        Duration::from_micros(375),
    ];
    assert_eq!(
        format_duration_summary(&samples).ok().as_deref(),
        Some("0.250[0.125,0.375]")
    );
}
