use std::io;

use super::super::super::DIRECT_PATH;
use super::types::{
    ArtifactMetadata, FixtureMetadata, MetricDelta, PerformanceArtifact, RawMetrics,
    StrategyConstants,
};
use crate::benchmark::BenchmarkResult;

fn metric(value: f64) -> f64 {
    (value * 1_000_000.0).round() / 1_000_000.0
}

pub(super) fn validate_metadata(
    baseline: &PerformanceArtifact,
    candidate_metadata: &ArtifactMetadata,
    candidate_fixture: &FixtureMetadata,
    candidate_strategy_constants: &StrategyConstants,
) -> BenchmarkResult<()> {
    if baseline.schema_version != 1 {
        return Err(io::Error::other(
            "benchmark artifact schema version is not supported",
        ));
    }
    if baseline.fixture != candidate_fixture.clone() {
        return Err(io::Error::other(
            "benchmark artifact fixture fingerprint or source metadata differs",
        ));
    }
    let same_comparison_metadata = baseline.metadata.rust_toolchain
        == candidate_metadata.rust_toolchain
        && baseline.metadata.target_platform == candidate_metadata.target_platform
        && baseline.metadata.cargo_profile == candidate_metadata.cargo_profile
        && baseline.metadata.model == candidate_metadata.model
        && baseline.metadata.runs == candidate_metadata.runs
        && baseline.metadata.warmups == candidate_metadata.warmups;
    if !same_comparison_metadata {
        return Err(io::Error::other(
            "benchmark artifact model, toolchain, target, profile, runs, or warmups metadata differs",
        ));
    }
    if baseline.strategy_constants != candidate_strategy_constants.clone() {
        return Err(io::Error::other(
            "benchmark artifact strategy constants differ",
        ));
    }
    Ok(())
}

pub(super) fn deltas(
    baseline: &RawMetrics,
    candidate: &RawMetrics,
) -> BenchmarkResult<Vec<MetricDelta>> {
    if baseline.cases.is_empty() || candidate.cases.is_empty() {
        return Err(io::Error::other(
            "benchmark artifact comparison requires baseline and candidate metrics",
        ));
    }
    let direct_baseline = baseline.cases.iter().all(|case| case.path == DIRECT_PATH);
    candidate
        .cases
        .iter()
        .filter(|case| !direct_baseline || case.path != DIRECT_PATH)
        .map(|case| {
            let baseline_case = baseline
                .cases
                .iter()
                .find(|candidate| {
                    candidate.scenario == case.scenario
                        && (candidate.path == case.path
                            || (direct_baseline && candidate.path == DIRECT_PATH))
                })
                .ok_or_else(|| {
                    io::Error::other(format!(
                        "benchmark artifact baseline is missing scenario={} path={}",
                        case.scenario, case.path
                    ))
                })?;
            Ok(MetricDelta {
                scenario: case.scenario.clone(),
                path: case.path.clone(),
                e2e_median_ms: metric(
                    case.summary.e2e_median_ms - baseline_case.summary.e2e_median_ms,
                ),
                ttft_median_ms: match (
                    case.summary.ttft_median_ms,
                    baseline_case.summary.ttft_median_ms,
                ) {
                    (Some(candidate), Some(baseline)) => Some(metric(candidate - baseline)),
                    _ => None,
                },
                raw_bytes: signed_delta(
                    case.summary.raw_bytes.min,
                    baseline_case.summary.raw_bytes.min,
                ),
                encoded_bytes: signed_delta(
                    case.summary.encoded_bytes.min,
                    baseline_case.summary.encoded_bytes.min,
                ),
                compression_queue_wait_ms: signed_float_delta(
                    case.summary.compression_metrics.queue_wait_ms,
                    baseline_case.summary.compression_metrics.queue_wait_ms,
                ),
                compression_work_time_ms: signed_float_delta(
                    case.summary.compression_metrics.work_time_ms,
                    baseline_case.summary.compression_metrics.work_time_ms,
                ),
                compression_failures: signed_optional_delta(
                    case.summary.compression_metrics.failures,
                    baseline_case.summary.compression_metrics.failures,
                ),
                connection_reconnects: signed_optional_delta(
                    case.summary
                        .connection_churn
                        .websocket_reconnects
                        .as_ref()
                        .map(|range| range.max),
                    baseline_case
                        .summary
                        .connection_churn
                        .websocket_reconnects
                        .as_ref()
                        .map(|range| range.max),
                ),
            })
        })
        .collect()
}

fn signed_float_delta(candidate: Option<f64>, baseline: Option<f64>) -> Option<f64> {
    match (candidate, baseline) {
        (Some(candidate), Some(baseline)) => Some(metric(candidate - baseline)),
        (Some(candidate), None) => Some(metric(candidate)),
        (None, Some(baseline)) => Some(metric(-baseline)),
        (None, None) => None,
    }
}

fn signed_optional_delta(candidate: Option<u64>, baseline: Option<u64>) -> Option<i64> {
    match (candidate, baseline) {
        (Some(candidate), Some(baseline)) => Some(signed_delta(candidate, baseline)),
        (Some(candidate), None) => Some(i64::try_from(candidate).unwrap_or(i64::MAX)),
        (None, Some(baseline)) => Some(-i64::try_from(baseline).unwrap_or(i64::MAX)),
        (None, None) => None,
    }
}

pub(super) fn require_case_shape(
    baseline: &RawMetrics,
    candidate: &RawMetrics,
) -> BenchmarkResult<()> {
    if baseline.cases.len() != candidate.cases.len()
        || baseline.cases.iter().any(|baseline_case| {
            !candidate.cases.iter().any(|candidate_case| {
                candidate_case.scenario == baseline_case.scenario
                    && candidate_case.path == baseline_case.path
            })
        })
    {
        return Err(io::Error::other(
            "benchmark artifact baseline and candidate case shape differs",
        ));
    }
    Ok(())
}

fn signed_delta(candidate: u64, baseline: u64) -> i64 {
    i64::try_from(candidate).unwrap_or(i64::MAX) - i64::try_from(baseline).unwrap_or(i64::MAX)
}
