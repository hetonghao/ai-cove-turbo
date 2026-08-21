use std::{io, time::Duration};

use super::super::{artifact_json, build_artifact};
use super::{cases, settings};
use crate::benchmark::BenchmarkResult;
use crate::benchmark::{CompressionSampleMetrics, DIRECT_PATH, HTTP_PATH};

#[test]
fn marks_repeated_median_latency_regression_without_gain_threshold() -> BenchmarkResult<()> {
    let baseline = build_artifact(&settings("fixture"), &cases(), None)?;
    let mut candidate_cases = cases();
    for sample in &mut candidate_cases
        .get_mut(1)
        .ok_or_else(|| io::Error::other("candidate HTTP case missing"))?
        .samples
    {
        sample.e2e += Duration::from_millis(100);
        sample.first_events = vec![Duration::from_millis(100)];
    }
    let candidate = build_artifact(&settings("fixture"), &candidate_cases, Some(&baseline))?;
    assert_eq!(candidate.judgement.status, "regression");
    assert!(candidate.judgement.comparable);
    Ok(())
}

#[test]
fn marks_byte_compression_and_connection_regression() -> BenchmarkResult<()> {
    let baseline = build_artifact(&settings("fixture"), &cases(), None)?;
    let mut candidate_cases = cases();
    for sample in &mut candidate_cases
        .get_mut(1)
        .ok_or_else(|| io::Error::other("candidate HTTP case missing"))?
        .samples
    {
        sample.raw_bytes = 3_000;
        sample.encoded_bytes = 2_000;
        sample.websocket_reconnects = 2;
        sample.messages_per_connection = Some(1);
        sample.compression_metrics = Some(CompressionSampleMetrics {
            encode_count: 1,
            decode_count: 1,
            queue_wait_ms: 10,
            work_time_ms: 20,
            failures: 1,
            fast_path_count: 0,
        });
    }
    let candidate = build_artifact(&settings("fixture"), &candidate_cases, Some(&baseline))?;
    assert_eq!(candidate.judgement.status, "regression");
    let delta = candidate
        .delta
        .iter()
        .find(|delta| delta.path == "Turbo HTTP + 自适应 zstd")
        .ok_or_else(|| io::Error::other("HTTP delta missing"))?;
    assert!(delta.raw_bytes > 0);
    assert!(delta.encoded_bytes > 0);
    assert!(
        delta
            .compression_queue_wait_ms
            .is_some_and(|value| value > 0.0)
    );
    assert!(
        delta
            .compression_work_time_ms
            .is_some_and(|value| value > 0.0)
    );
    assert!(delta.compression_failures.is_some_and(|value| value > 0));
    assert!(delta.connection_reconnects.is_some_and(|value| value > 0));
    Ok(())
}

#[test]
fn rejects_comparison_when_fixture_metadata_differs() -> BenchmarkResult<()> {
    let baseline = build_artifact(&settings("fixture-a"), &cases(), None)?;
    let candidate = build_artifact(&settings("fixture-b"), &cases(), None)?;
    let error = super::super::compare::validate_metadata(
        &baseline,
        &candidate.metadata,
        &candidate.fixture,
        &candidate.strategy_constants,
    )
    .expect_err("different fixture fingerprints must reject comparison")
    .to_string();
    assert!(error.contains("fixture fingerprint"));
    Ok(())
}

#[test]
fn rejects_comparison_when_runs_metadata_differs() -> BenchmarkResult<()> {
    let baseline = build_artifact(&settings("fixture"), &cases(), None)?;
    let mut candidate_settings = settings("fixture");
    candidate_settings.runs = 8;
    let candidate = build_artifact(&candidate_settings, &cases(), None)?;
    let error = super::super::compare::validate_metadata(
        &baseline,
        &candidate.metadata,
        &candidate.fixture,
        &candidate.strategy_constants,
    )
    .expect_err("different run counts must reject comparison")
    .to_string();
    assert!(error.contains("runs"));
    Ok(())
}

#[test]
fn allows_turbo_sha_to_change_between_baseline_and_candidate() -> BenchmarkResult<()> {
    let mut baseline = build_artifact(&settings("fixture"), &cases(), None)?;
    baseline.metadata.turbo_sha = "baseline-sha".to_owned();
    let mut candidate = build_artifact(&settings("fixture"), &cases(), None)?;
    candidate.metadata.turbo_sha = "candidate-sha".to_owned();
    super::super::compare::validate_metadata(
        &baseline,
        &candidate.metadata,
        &candidate.fixture,
        &candidate.strategy_constants,
    )?;
    Ok(())
}

#[test]
fn rejects_artifact_without_direct_fixture_baseline() {
    let cases = vec![super::super::super::BenchmarkCase {
        scenario: "fixture",
        path: HTTP_PATH,
        samples: vec![super::sample(90)],
    }];
    let error = build_artifact(&settings("fixture"), &cases, None)
        .expect_err("comparison requires a fixed direct fixture baseline")
        .to_string();
    assert!(error.contains("direct baseline"));
}

#[test]
fn rejects_empty_fixture() {
    let error = build_artifact(&settings(""), &cases(), None)
        .expect_err("empty fixture must not produce comparable evidence")
        .to_string();
    assert!(error.contains("fixture must not be empty"));
}

#[test]
fn rejects_comparison_when_case_shape_differs() -> BenchmarkResult<()> {
    let baseline = build_artifact(&settings("fixture"), &cases(), None)?;
    let candidate_cases = vec![super::super::super::BenchmarkCase {
        scenario: "fixture",
        path: DIRECT_PATH,
        samples: vec![super::sample(90)],
    }];
    let error = build_artifact(&settings("fixture"), &candidate_cases, Some(&baseline))
        .expect_err("missing candidate paths must reject comparison")
        .to_string();
    assert!(error.contains("case shape differs"));
    Ok(())
}

#[test]
fn comparison_json_remains_serializable_after_judgement() -> BenchmarkResult<()> {
    let artifact = build_artifact(&settings("fixture"), &cases(), None)?;
    let json = artifact_json(&artifact)?;
    assert!(json.contains("\"judgement\"") && json.contains("\"status\""));
    Ok(())
}
