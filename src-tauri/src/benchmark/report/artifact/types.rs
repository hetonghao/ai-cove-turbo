use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PerformanceArtifact {
    pub(super) schema_version: u32,
    pub(super) metadata: ArtifactMetadata,
    pub(super) fixture: FixtureMetadata,
    pub(super) strategy_constants: StrategyConstants,
    pub(super) baseline: RawMetrics,
    pub(super) candidate: RawMetrics,
    pub(super) delta: Vec<MetricDelta>,
    pub(super) judgement: Judgement,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ArtifactMetadata {
    pub(super) turbo_sha: String,
    pub(super) rust_toolchain: String,
    pub(super) target_platform: String,
    pub(super) cargo_profile: String,
    pub(super) model: String,
    pub(super) runs: usize,
    pub(super) warmups: usize,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct FixtureMetadata {
    pub(super) fingerprint: String,
    pub(super) source: String,
    pub(super) bytes: usize,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct StrategyConstants {
    pub(super) max_output_tokens: u64,
    pub(super) compression_level: u8,
    pub(super) min_compression_input_bytes: u64,
    pub(super) max_request_body_bytes: u64,
    pub(super) reference_uplink_mbps: f64,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RawMetrics {
    pub(super) cases: Vec<CaseMetrics>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CaseMetrics {
    pub(super) scenario: String,
    pub(super) path: String,
    pub(super) summary: SummaryMetrics,
    pub(super) samples: Vec<SampleMetrics>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SummaryMetrics {
    pub(super) e2e_median_ms: f64,
    pub(super) ttft_median_ms: Option<f64>,
    pub(super) raw_bytes: CountMetrics,
    pub(super) encoded_bytes: CountMetrics,
    pub(super) compression_metrics: CompressionMetrics,
    pub(super) connection_churn: ConnectionChurnMetrics,
    pub(super) valid_samples: usize,
    pub(super) recovered_samples: usize,
    pub(super) retries: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CompressionMetrics {
    pub(super) source: String,
    pub(super) encode_count: Option<u64>,
    pub(super) decode_count: Option<u64>,
    pub(super) queue_wait_ms: Option<f64>,
    pub(super) work_time_ms: Option<f64>,
    pub(super) failures: Option<u64>,
    pub(super) fast_path_count: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ConnectionChurnMetrics {
    pub(super) websocket_handshakes: CountMetrics,
    pub(super) websocket_reconnects: Option<CountMetrics>,
    pub(super) messages_per_connection: Option<CountMetrics>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CountMetrics {
    pub(super) min: u64,
    pub(super) max: u64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct SampleMetrics {
    pub(super) e2e_ms: f64,
    pub(super) setup_ms: f64,
    pub(super) first_events_ms: Vec<f64>,
    pub(super) round_e2e_ms: Vec<f64>,
    pub(super) warm_round_e2e_ms: Vec<f64>,
    pub(super) connection_lifetime_ms: Option<f64>,
    pub(super) websocket_reconnects: u64,
    pub(super) messages_per_connection: Option<u64>,
    pub(super) raw_bytes: u64,
    pub(super) encoded_bytes: u64,
    pub(super) logical_requests: u64,
    pub(super) application_messages: u64,
    pub(super) http_requests: u64,
    pub(super) websocket_messages: u64,
    pub(super) response_events: u64,
    pub(super) websocket_handshakes: u64,
    pub(super) retries: u64,
    pub(super) round_transports: Vec<String>,
    pub(super) compression_metrics: Option<CompressionMetrics>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct MetricDelta {
    pub(super) scenario: String,
    pub(super) path: String,
    pub(super) e2e_median_ms: f64,
    pub(super) ttft_median_ms: Option<f64>,
    pub(super) raw_bytes: i64,
    pub(super) encoded_bytes: i64,
    pub(super) compression_queue_wait_ms: Option<f64>,
    pub(super) compression_work_time_ms: Option<f64>,
    pub(super) compression_failures: Option<i64>,
    pub(super) connection_reconnects: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Judgement {
    pub(super) status: String,
    pub(super) baseline_source: String,
    pub(super) comparable: bool,
    pub(super) reasons: Vec<String>,
}
