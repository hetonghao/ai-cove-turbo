use std::{io, net::IpAddr, time::Duration};

use serde::Deserialize;
use url::Url;

mod live;
mod measure;
mod observe;

const ACK_PATH: &str = "/transport/ack";
const ACK_TIMEOUT: Duration = Duration::from_secs(15);
const FIXTURE_TARGET_BYTES: usize = 64 * 1024;
const FIXTURE_PATTERN: &str = "transport-ack-issue-14;0123456789abcdef\n";
const ACK_KEYS: [&str; 6] = [
    "ok",
    "transport",
    "wire_bytes",
    "decoded_bytes",
    "receive_ms",
    "decode_ms",
];

#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct TransportAck {
    ok: bool,
    transport: String,
    wire_bytes: u64,
    decoded_bytes: u64,
    receive_ms: f64,
    decode_ms: f64,
}

fn parse_ack(payload: &[u8]) -> Result<TransportAck, io::Error> {
    let value = serde_json::from_slice::<serde_json::Value>(payload)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let Some(object) = value.as_object() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "transport ACK must be a JSON object",
        ));
    };
    if object.len() != ACK_KEYS.len() || ACK_KEYS.iter().any(|key| !object.contains_key(*key)) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "transport ACK JSON keys do not match the approved six-key contract",
        ));
    }
    let ack = serde_json::from_value::<TransportAck>(value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if !ack.receive_ms.is_finite()
        || ack.receive_ms < 0.0
        || !ack.decode_ms.is_finite()
        || ack.decode_ms < 0.0
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "transport ACK timing fields must be finite non-negative milliseconds",
        ));
    }
    Ok(ack)
}

fn ack_url(base: &str, websocket: bool) -> Result<String, io::Error> {
    let mut url =
        Url::parse(base).map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let host = url.host_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "transport ACK upstream needs a host",
        )
    })?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    if !loopback {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "transport ACK benchmark only permits loopback upstreams",
        ));
    }
    let base_path = url.path().trim_end_matches('/');
    url.set_path(&format!("{base_path}{ACK_PATH}"));
    url.set_query(None);
    let scheme = match (websocket, url.scheme()) {
        (true, "http") => "ws".to_owned(),
        (true, "https") => "wss".to_owned(),
        (false, "http" | "https") => url.scheme().to_owned(),
        (true, _) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "WebSocket ACK upstream must use http(s)",
            ));
        }
        (false, _) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "HTTP ACK upstream must use http(s)",
            ));
        }
    };
    url.set_scheme(&scheme).map_err(|()| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "transport ACK scheme is invalid",
        )
    })?;
    Ok(url.to_string())
}

fn fixture_payload() -> String {
    let mut data = String::with_capacity(FIXTURE_TARGET_BYTES);
    while data.len() < FIXTURE_TARGET_BYTES {
        data.push_str(FIXTURE_PATTERN);
    }
    serde_json::json!({
        "kind": "transport-ack",
        "fixture": "issue-14-v1",
        "data": data,
    })
    .to_string()
}

fn validate_ack(
    ack: &TransportAck,
    expected_transport: &str,
    expected_decoded_bytes: u64,
) -> Result<(), io::Error> {
    if !ack.ok {
        return Err(io::Error::other("transport ACK returned ok=false"));
    }
    if ack.transport != expected_transport {
        return Err(io::Error::other(
            "transport ACK transport field is incorrect",
        ));
    }
    if ack.decoded_bytes != expected_decoded_bytes {
        return Err(io::Error::other(
            "transport ACK decoded_bytes does not match the application payload",
        ));
    }
    Ok(())
}

fn validate_identity_ack(
    ack: &TransportAck,
    expected_transport: &str,
    payload_bytes: u64,
) -> Result<(), io::Error> {
    validate_ack(ack, expected_transport, payload_bytes)?;
    if ack.wire_bytes != payload_bytes {
        return Err(io::Error::other(
            "direct transport ACK wire_bytes does not match the application payload",
        ));
    }
    Ok(())
}

fn validate_metric_correlation(
    ack: &TransportAck,
    raw_bytes: u64,
    sent_bytes: u64,
) -> Result<(), io::Error> {
    if ack.decoded_bytes != raw_bytes || ack.wire_bytes != sent_bytes {
        return Err(io::Error::other(
            "Turbo byte metrics do not match the New API transport ACK",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::{
        FIXTURE_TARGET_BYTES, TransportAck, ack_url, fixture_payload, parse_ack, validate_ack,
        validate_identity_ack, validate_metric_correlation,
    };

    const VALID_ACK: &[u8] = br#"{"ok":true,"transport":"http","wire_bytes":64,"decoded_bytes":64,"receive_ms":0.1,"decode_ms":0.2}"#;

    #[test]
    fn parses_exact_six_key_transport_ack() {
        let result = parse_ack(VALID_ACK);
        assert!(result.is_ok(), "approved six-key ACK must parse");
        let Some(ack) = result.ok() else {
            return;
        };
        assert_eq!(
            ack,
            TransportAck {
                ok: true,
                transport: "http".to_owned(),
                wire_bytes: 64,
                decoded_bytes: 64,
                receive_ms: 0.1,
                decode_ms: 0.2,
            }
        );
    }

    #[test]
    fn rejects_missing_or_unknown_transport_ack_keys() {
        let missing = br#"{"ok":true,"transport":"http","wire_bytes":64,"decoded_bytes":64,"receive_ms":0.1}"#;
        let unknown = br#"{"ok":true,"transport":"http","wire_bytes":64,"decoded_bytes":64,"receive_ms":0.1,"decode_ms":0.2,"extra":true}"#;
        assert!(parse_ack(missing).is_err());
        assert!(parse_ack(unknown).is_err());
    }

    #[test]
    fn rejects_non_loopback_upstream_and_builds_http_and_ws_urls() {
        assert!(ack_url("https://api.ai-cove.com/v1", false).is_err());
        assert_eq!(
            ack_url("http://127.0.0.1:3000/v1", false).ok().as_deref(),
            Some("http://127.0.0.1:3000/v1/transport/ack")
        );
        let ipv6 = ack_url("http://[::1]:3000/v1", true);
        assert!(ipv6.is_ok(), "loopback IPv6 URL must be accepted: {ipv6:?}");
        assert_eq!(
            ipv6.ok().as_deref(),
            Some("ws://[::1]:3000/v1/transport/ack")
        );
    }

    #[test]
    fn validates_direct_http_and_websocket_field_mapping() {
        let payload_bytes = 64;
        let http = TransportAck {
            ok: true,
            transport: "http".to_owned(),
            wire_bytes: payload_bytes,
            decoded_bytes: payload_bytes,
            receive_ms: 0.0,
            decode_ms: 0.0,
        };
        let websocket = TransportAck {
            transport: "websocket".to_owned(),
            ..http
        };
        assert!(validate_identity_ack(&http, "http", payload_bytes).is_ok());
        assert!(validate_identity_ack(&websocket, "websocket", payload_bytes).is_ok());
    }

    #[test]
    fn correlates_ack_bytes_to_turbo_raw_and_sent_metrics() {
        let ack = TransportAck {
            ok: true,
            transport: "websocket".to_owned(),
            wire_bytes: 42,
            decoded_bytes: 128,
            receive_ms: 0.5,
            decode_ms: 0.25,
        };
        assert!(validate_ack(&ack, "websocket", 128).is_ok());
        assert!(validate_metric_correlation(&ack, 128, 42).is_ok());
        assert!(validate_metric_correlation(&ack, 127, 42).is_err());
    }

    #[test]
    fn fixture_is_long_and_zstd_compressible() {
        let payload = fixture_payload();
        assert!(payload.len() >= FIXTURE_TARGET_BYTES);
        let encoded = zstd::stream::encode_all(Cursor::new(payload.as_bytes()), 3);
        assert!(encoded.is_ok());
        let Some(encoded) = encoded.ok() else {
            return;
        };
        assert!(encoded.len() < payload.len());
    }
}
