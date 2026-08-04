use super::TransportAck;

#[derive(Debug)]
pub(super) struct AckObservation {
    pub(super) ack: TransportAck,
    pub(super) client_ack_rtt_ms: f64,
    pub(super) websocket_setup_ms: Option<f64>,
}

impl AckObservation {
    pub(super) fn http(ack: TransportAck, client_ack_rtt_ms: f64) -> Self {
        Self {
            ack,
            client_ack_rtt_ms,
            websocket_setup_ms: None,
        }
    }

    pub(super) fn websocket(
        ack: TransportAck,
        client_ack_rtt_ms: f64,
        websocket_setup_ms: f64,
    ) -> Self {
        Self {
            ack,
            client_ack_rtt_ms,
            websocket_setup_ms: Some(websocket_setup_ms),
        }
    }
}

pub(super) fn render_observation(path: &str, observation: &AckObservation) -> String {
    let websocket_setup_ms = observation
        .websocket_setup_ms
        .map_or_else(|| "—".to_owned(), |value| format!("{value:.3}"));
    let ack = &observation.ack;
    format!(
        "path={path} client_ack_rtt_ms={:.3} websocket_setup_ms={websocket_setup_ms} receive_ms={:.3} decode_ms={:.3} wire_bytes={} decoded_bytes={}",
        observation.client_ack_rtt_ms,
        ack.receive_ms,
        ack.decode_ms,
        ack.wire_bytes,
        ack.decoded_bytes,
    )
}

pub(super) fn print_observation(path: &str, observation: &AckObservation) {
    println!("{}", render_observation(path, observation));
}

#[cfg(test)]
mod tests {
    use super::{super::TransportAck, render_observation};

    #[test]
    fn renders_observation_contract_without_payload_or_token() {
        let observation = TransportAck {
            ok: true,
            transport: "http".to_owned(),
            wire_bytes: 42,
            decoded_bytes: 64,
            receive_ms: 0.125,
            decode_ms: 0.25,
        };
        let line = render_observation(
            "direct_http",
            &super::AckObservation {
                ack: observation,
                client_ack_rtt_ms: 1.234,
                websocket_setup_ms: None,
            },
        );
        assert_eq!(
            line,
            "path=direct_http client_ack_rtt_ms=1.234 websocket_setup_ms=— receive_ms=0.125 decode_ms=0.250 wire_bytes=42 decoded_bytes=64"
        );
        assert!(!line.contains("transport-ack-issue-14"));
        assert!(!line.contains("Bearer"));

        let websocket_line = render_observation(
            "turbo_websocket",
            &super::AckObservation {
                ack: TransportAck {
                    ok: true,
                    transport: "websocket".to_owned(),
                    wire_bytes: 12,
                    decoded_bytes: 64,
                    receive_ms: 0.5,
                    decode_ms: 0.25,
                },
                client_ack_rtt_ms: 3.456,
                websocket_setup_ms: Some(2.345),
            },
        );
        assert_eq!(
            websocket_line,
            "path=turbo_websocket client_ack_rtt_ms=3.456 websocket_setup_ms=2.345 receive_ms=0.500 decode_ms=0.250 wire_bytes=12 decoded_bytes=64"
        );
    }
}
