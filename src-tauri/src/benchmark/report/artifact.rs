use std::{env, fs, io};

use super::super::{BenchmarkCase, BenchmarkResult, BenchmarkSettings, DIRECT_PATH};
use types::{Judgement, MetricDelta, PerformanceArtifact, RawMetrics};

mod compare;
mod metadata;
mod metrics;
mod types;

const SCHEMA_VERSION: u32 = 1;

pub(super) fn write_if_requested(
    settings: &BenchmarkSettings,
    cases: &[BenchmarkCase],
) -> BenchmarkResult<()> {
    let Some(output) = env::var_os("TURBO_METRICS_OUT") else {
        return Ok(());
    };
    let baseline = env::var_os("TURBO_METRICS_BASELINE")
        .map(fs::read)
        .transpose()?
        .as_deref()
        .map(|bytes| serde_json::from_slice::<PerformanceArtifact>(bytes).map_err(io::Error::other))
        .transpose()?;
    let artifact = build_artifact(settings, cases, baseline.as_ref())?;
    fs::write(output, artifact_json(&artifact)?)?;
    Ok(())
}

fn build_artifact(
    settings: &BenchmarkSettings,
    cases: &[BenchmarkCase],
    baseline: Option<&PerformanceArtifact>,
) -> BenchmarkResult<PerformanceArtifact> {
    let candidate = metrics::raw_metrics(cases)?;
    let fixture = metadata::fixture(settings)?;
    let metadata = metadata::metadata(settings);
    let strategy_constants = metadata::strategy_constants();
    let (baseline, baseline_source) = match baseline {
        Some(baseline) => {
            compare::validate_metadata(baseline, &metadata, &fixture, &strategy_constants)?;
            compare::require_case_shape(&baseline.candidate, &candidate)?;
            (baseline.candidate.clone(), "artifact_candidate".to_owned())
        }
        None => (direct_baseline(&candidate)?, "direct_path".to_owned()),
    };
    let delta = compare::deltas(&baseline, &candidate)?;
    let judgement = judge(&candidate, &baseline_source, &delta);
    Ok(PerformanceArtifact {
        schema_version: SCHEMA_VERSION,
        metadata,
        fixture,
        strategy_constants,
        baseline,
        candidate,
        delta,
        judgement,
    })
}

fn judge(candidate: &RawMetrics, baseline_source: &str, delta: &[MetricDelta]) -> Judgement {
    let stable_regression = delta.iter().any(|delta| {
        let Some(case) = candidate
            .cases
            .iter()
            .find(|case| case.scenario == delta.scenario && case.path == delta.path)
        else {
            return false;
        };
        let ttft_regressed = delta.ttft_median_ms.is_none_or(|value| value > 0.0);
        let bytes_regressed = delta.raw_bytes > 0 || delta.encoded_bytes > 0;
        let prior_artifact = baseline_source == "artifact_candidate";
        let compression_regressed = prior_artifact
            && (delta
                .compression_queue_wait_ms
                .is_some_and(|value| value > 0.0)
                || delta
                    .compression_work_time_ms
                    .is_some_and(|value| value > 0.0)
                || delta.compression_failures.is_some_and(|value| value > 0));
        let connection_regressed =
            prior_artifact && delta.connection_reconnects.is_some_and(|value| value > 0);
        case.summary.valid_samples >= 2
            && (delta.e2e_median_ms > 0.0
                || ttft_regressed
                || bytes_regressed
                || compression_regressed
                || connection_regressed)
    });
    let status = if stable_regression {
        "regression"
    } else {
        "pass"
    };
    let reason = if stable_regression {
        "A compared path has a repeated-sample key metric regression."
    } else {
        "No compared path has a repeated-sample key metric regression."
    };
    Judgement {
        status: status.to_owned(),
        baseline_source: baseline_source.to_owned(),
        comparable: true,
        reasons: vec![
            reason.to_owned(),
            "No fixed improvement threshold is applied.".to_owned(),
        ],
    }
}

fn direct_baseline(candidate: &RawMetrics) -> BenchmarkResult<RawMetrics> {
    let cases = candidate
        .cases
        .iter()
        .filter(|case| case.path == DIRECT_PATH)
        .cloned()
        .collect::<Vec<_>>();
    if cases.is_empty() {
        return Err(io::Error::other(
            "benchmark artifact is missing its direct baseline",
        ));
    }
    Ok(RawMetrics { cases })
}

fn artifact_json(artifact: &PerformanceArtifact) -> BenchmarkResult<String> {
    serde_json::to_string_pretty(artifact).map_err(io::Error::other)
}

#[cfg(test)]
#[path = "artifact_tests.rs"]
mod tests;
