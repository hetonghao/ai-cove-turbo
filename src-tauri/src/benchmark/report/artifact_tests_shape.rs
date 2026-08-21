use std::io;

use serde_json::Value;

use super::super::{artifact_json, build_artifact};
use super::{cases, settings};
use crate::benchmark::BenchmarkResult;
use crate::benchmark::CompressionSampleMetrics;

#[test]
#[allow(clippy::indexing_slicing)]
fn emits_versioned_shape_without_sensitive_fixture_or_auth_data() -> BenchmarkResult<()> {
    let artifact = build_artifact(
        &settings("secret request body that must stay out of evidence"),
        &cases(),
        None,
    )?;
    let json = artifact_json(&artifact)?;
    let value: Value = serde_json::from_str(&json).map_err(io::Error::other)?;

    for field in [
        "schemaVersion",
        "metadata",
        "fixture",
        "strategyConstants",
        "baseline",
        "candidate",
        "delta",
        "judgement",
    ] {
        assert!(value.get(field).is_some(), "missing JSON field {field}");
    }
    assert_eq!(value.get("schemaVersion"), Some(&Value::from(1)));
    let metadata = value
        .get("metadata")
        .ok_or_else(|| io::Error::other("metadata missing"))?;
    assert_eq!(metadata.get("runs"), Some(&Value::from(4)));
    assert_eq!(metadata.get("warmups"), Some(&Value::from(1)));
    assert_eq!(metadata.get("model"), Some(&Value::from("fixture-model")));
    assert_eq!(value["judgement"]["status"], "pass");
    assert!(value["fixture"]["fingerprint"].as_str().is_some());
    assert!(value["candidate"]["cases"].as_array().is_some());
    assert!(value["delta"].as_array().is_some());
    let summary = value
        .pointer("/candidate/cases/0/summary")
        .ok_or_else(|| io::Error::other("candidate summary missing"))?;
    assert_eq!(summary["compressionMetrics"]["source"], "not_applicable");
    for field in [
        "encodeCount",
        "decodeCount",
        "queueWaitMs",
        "workTimeMs",
        "failures",
        "fastPathCount",
    ] {
        assert!(summary["compressionMetrics"][field].is_null());
    }
    assert!(summary["connectionChurn"]["websocketHandshakes"].is_object());
    assert!(summary["connectionChurn"]["websocketReconnects"].is_null());
    assert!(summary["connectionChurn"]["messagesPerConnection"].is_null());
    assert!(!json.contains("secret request body"));
    assert!(!json.contains("Bearer "));
    assert!(!json.contains("api_key"));
    Ok(())
}

#[test]
#[allow(clippy::indexing_slicing)]
fn serializes_real_compression_delta_metrics_when_available() -> BenchmarkResult<()> {
    let mut cases = cases();
    for sample in &mut cases
        .get_mut(0)
        .ok_or_else(|| io::Error::other("direct case missing"))?
        .samples
    {
        sample.compression_metrics = Some(CompressionSampleMetrics {
            encode_count: 2,
            decode_count: 1,
            queue_wait_ms: 3,
            work_time_ms: 4,
            failures: 5,
            fast_path_count: 6,
        });
    }
    for sample in &mut cases
        .get_mut(1)
        .ok_or_else(|| io::Error::other("HTTP case missing"))?
        .samples
    {
        sample.websocket_handshakes = 2;
        sample.websocket_reconnects = 1;
        sample.messages_per_connection = Some(3);
    }
    let artifact = build_artifact(&settings("fixture"), &cases, None)?;
    let value: Value =
        serde_json::from_str(&artifact_json(&artifact)?).map_err(io::Error::other)?;
    let metrics = value
        .pointer("/candidate/cases/0/summary/compressionMetrics")
        .ok_or_else(|| io::Error::other("compression metrics missing"))?;
    assert_eq!(metrics["source"], "metrics_snapshot_delta");
    assert_eq!(metrics["encodeCount"], 4);
    assert_eq!(metrics["decodeCount"], 2);
    assert_eq!(metrics["queueWaitMs"], 6.0);
    assert_eq!(metrics["workTimeMs"], 8.0);
    assert_eq!(metrics["failures"], 10);
    assert_eq!(metrics["fastPathCount"], 12);
    let churn = value
        .pointer("/candidate/cases/1/summary/connectionChurn")
        .ok_or_else(|| io::Error::other("connection churn missing"))?;
    assert_eq!(churn["websocketHandshakes"]["min"], 2);
    assert_eq!(churn["websocketReconnects"]["max"], 1);
    assert_eq!(churn["messagesPerConnection"]["min"], 3);
    Ok(())
}

#[test]
fn artifact_round_trips_without_shape_loss() -> BenchmarkResult<()> {
    let artifact = build_artifact(&settings("fixture"), &cases(), None)?;
    let json = artifact_json(&artifact)?;
    let decoded: super::super::types::PerformanceArtifact =
        serde_json::from_str(&json).map_err(io::Error::other)?;
    assert_eq!(decoded, artifact);
    Ok(())
}
