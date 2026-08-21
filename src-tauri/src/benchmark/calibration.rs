use std::{
    collections::HashSet,
    fs,
    io::{self, Write},
    path::Path,
    time::Duration,
};

use serde::{Deserialize, Serialize};

use super::{
    BenchmarkCase, BenchmarkSettings, DIRECT_PATH, HTTP_PATH, HYBRID_PATH, WEBSOCKET_PATH,
    report::case_report, workload_fingerprint,
};

const PROFILE_SCHEMA_VERSION: u32 = 1;
const MIN_HISTORY_DATES: usize = 3;
const TARGET_HISTORY_DATES: usize = 5;
const MIN_MATCH_COVERAGE_PCT: f64 = 70.0;
const MIN_P90_BUCKET_SAMPLES: u64 = 12;

#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(
    deny_unknown_fields,
    rename_all(deserialize = "snake_case", serialize = "camelCase")
)]
struct BucketKey {
    input: usize,
    output: usize,
    cached_ratio: usize,
    reasoning_effort: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileFilters {
    model: String,
    channel: String,
    endpoint: String,
    stream: bool,
    same_window: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BucketBoundaries {
    input_tokens: Vec<u64>,
    output_tokens: Vec<u64>,
    cached_ratio: Vec<f64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CurrentBucket {
    bucket: BucketKey,
    count: u64,
    http_requests: u64,
    websocket_requests: u64,
    raw_bytes_p50: u64,
    raw_bytes_p90: u64,
    sent_bytes_p50: u64,
    sent_bytes_p90: u64,
    first_event_p50_ms: f64,
    first_event_p90_ms: f64,
    complete_p50_ms: f64,
    complete_p90_ms: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoricalBucket {
    bucket: BucketKey,
    count: u64,
    first_event_p50_ms: f64,
    first_event_p90_ms: Option<f64>,
    complete_p50_ms: f64,
    complete_p90_ms: Option<f64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoricalDay {
    date: String,
    same_window: Vec<HistoricalBucket>,
    full_day: Vec<HistoricalBucket>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MissingReason {
    InsufficientSamples,
    TurboPresent,
    FilterMismatch,
    DataUnavailable,
}

impl MissingReason {
    const fn label(self) -> &'static str {
        match self {
            Self::InsufficientSamples => "insufficient_samples",
            Self::TurboPresent => "turbo_present",
            Self::FilterMismatch => "filter_mismatch",
            Self::DataUnavailable => "data_unavailable",
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MissingDate {
    date: String,
    reason: MissingReason,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
// CLIPPY-ALLOW: Field names mirror the persisted benchmark constants schema.
#[allow(clippy::struct_field_names)]
struct SpeedConstants {
    baseline_first_token_ms: u64,
    baseline_complete_ms: u64,
    websocket_first_token_saved_ms: u64,
    websocket_complete_saved_ms: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkloadProfile {
    schema_version: u32,
    timezone: String,
    measurement_date: String,
    anchor_date: String,
    filters: ProfileFilters,
    bucket_boundaries: BucketBoundaries,
    old_constants: SpeedConstants,
    current: Vec<CurrentBucket>,
    history: Vec<HistoricalDay>,
    missing_dates: Vec<MissingDate>,
}

#[derive(Clone, Copy, Debug)]
// CLIPPY-ALLOW: Field names distinguish event and completion latency statistics.
#[allow(clippy::struct_field_names)]
struct LatencyCalibration {
    first_event_p50_ms: f64,
    first_event_p90_ms: Option<f64>,
    complete_p50_ms: f64,
    complete_p90_ms: Option<f64>,
}

#[derive(Clone, Debug)]
struct ScopeCalibration {
    sample_count: u64,
    covered_current_count: u64,
    coverage_pct: f64,
    latency: LatencyCalibration,
    buckets: Vec<BucketEvidence>,
}

#[derive(Clone, Debug)]
struct DayCalibration {
    date: String,
    same_window: ScopeCalibration,
    full_day: ScopeCalibration,
}

#[derive(Debug)]
struct ProfileCalibration {
    profile_fingerprint: String,
    days: Vec<DayCalibration>,
    current: LatencyCalibration,
    baseline_same_window: LatencyCalibration,
    baseline_full_day: LatencyCalibration,
    profile: WorkloadProfile,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
// CLIPPY-ALLOW: Field names preserve the HTTP and WebSocket evidence schema.
#[allow(clippy::struct_field_names)]
struct MechanismEvidence {
    http_first_event_ms: f64,
    websocket_first_event_ms: f64,
    http_complete_ms: f64,
    websocket_complete_ms: f64,
    paired_first_event_saved_ms: f64,
    paired_first_event_drift_pct: f64,
    paired_complete_saved_ms: f64,
    paired_complete_drift_pct: Option<f64>,
    complete_constant_qualified: bool,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ConstantChange {
    old_ms: u64,
    candidate_ms: u64,
    absolute_ms: i64,
    percent: f64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
// CLIPPY-ALLOW: Field names mirror the four public benchmark constants.
#[allow(clippy::struct_field_names)]
struct ConstantChanges {
    baseline_first_token_ms: ConstantChange,
    baseline_complete_ms: ConstantChange,
    websocket_first_token_saved_ms: ConstantChange,
    websocket_complete_saved_ms: ConstantChange,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CompressionSensitivity {
    uplink_mbps: f64,
    upload_saved_ms: f64,
    estimated_first_event_gain_pct: f64,
    estimated_complete_gain_pct: f64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct HistoricalObservation {
    first_event_gain_pct: f64,
    complete_gain_pct: f64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProfileFilterEvidence {
    model: String,
    channel: String,
    endpoint: String,
    stream: bool,
    same_window: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BucketBoundaryEvidence {
    input_tokens: Vec<u64>,
    output_tokens: Vec<u64>,
    cached_ratio: Vec<f64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MissingDateEvidence {
    date: String,
    reason: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkloadProfileEvidence {
    fingerprint: String,
    timezone: String,
    measurement_date: String,
    anchor_date: String,
    history_dates: Vec<String>,
    missing_dates: Vec<MissingDateEvidence>,
    filters: ProfileFilterEvidence,
    bucket_boundaries: BucketBoundaryEvidence,
    current_samples: u64,
    http_requests: u64,
    websocket_requests: u64,
}

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
// CLIPPY-ALLOW: Field names preserve the latency evidence report schema.
#[allow(clippy::struct_field_names)]
struct LatencyEvidence {
    first_event_p50_ms: f64,
    first_event_p90_ms: Option<f64>,
    complete_p50_ms: f64,
    complete_p90_ms: Option<f64>,
}

impl From<LatencyCalibration> for LatencyEvidence {
    fn from(value: LatencyCalibration) -> Self {
        Self {
            first_event_p50_ms: value.first_event_p50_ms,
            first_event_p90_ms: value.first_event_p90_ms,
            complete_p50_ms: value.complete_p50_ms,
            complete_p90_ms: value.complete_p90_ms,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BucketEvidence {
    bucket: BucketKey,
    current_samples: u64,
    historical_samples: u64,
    profile_coverage_pct: f64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ScopeEvidence {
    sample_count: u64,
    covered_current_count: u64,
    coverage_pct: f64,
    #[serde(flatten)]
    latency: LatencyEvidence,
    buckets: Vec<BucketEvidence>,
}

impl From<&ScopeCalibration> for ScopeEvidence {
    fn from(value: &ScopeCalibration) -> Self {
        Self {
            sample_count: value.sample_count,
            covered_current_count: value.covered_current_count,
            coverage_pct: value.coverage_pct,
            latency: value.latency.into(),
            buckets: value.buckets.clone(),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DayEvidence {
    date: String,
    same_window: ScopeEvidence,
    full_day: ScopeEvidence,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct HistoricalCalibrationEvidence {
    current: LatencyEvidence,
    baseline_same_window: LatencyEvidence,
    baseline_full_day: LatencyEvidence,
    days: Vec<DayEvidence>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PathEvidence {
    scenario: &'static str,
    path: &'static str,
    valid_samples: usize,
    recovered_samples: usize,
    retries: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BenchmarkEvidence {
    upstream: String,
    model: String,
    runs: usize,
    warmups: usize,
    timeout_seconds: u64,
    workload_source: &'static str,
    workload_fingerprint: String,
    workload_bytes: usize,
    core_paths: Vec<PathEvidence>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CandidateSummary {
    schema_version: u32,
    status: &'static str,
    workload_profile: WorkloadProfileEvidence,
    benchmark: BenchmarkEvidence,
    historical_calibration: HistoricalCalibrationEvidence,
    old_constants: SpeedConstants,
    candidate_constants: SpeedConstants,
    changes: ConstantChanges,
    mechanism: MechanismEvidence,
    historical_observation: HistoricalObservation,
    compression_sensitivity: Vec<CompressionSensitivity>,
}

fn profile_error(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn count_as_f64(value: u64) -> f64 {
    let high = u32::try_from(value >> 32).unwrap_or_default();
    let low = u32::try_from(value & u64::from(u32::MAX)).unwrap_or_default();
    f64::from(high) * 4_294_967_296.0 + f64::from(low)
}

fn median(values: impl IntoIterator<Item = f64>) -> Option<f64> {
    let mut values = values.into_iter().collect::<Vec<_>>();
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    let upper = values.get(middle).copied()?;
    if values.len().is_multiple_of(2) {
        Some(values.get(middle.checked_sub(1)?)?.midpoint(upper))
    } else {
        Some(upper)
    }
}

fn validate_boundaries(boundaries: &BucketBoundaries) -> Result<(), io::Error> {
    if boundaries.input_tokens.is_empty()
        || !boundaries
            .input_tokens
            .windows(2)
            .all(|pair| matches!(pair, [left, right] if left < right))
    {
        return Err(profile_error(
            "input token boundaries must be non-empty and strictly increasing",
        ));
    }
    if boundaries.output_tokens != [100, 300, 1_000, 3_000] {
        return Err(profile_error(
            "output token boundaries must be [100, 300, 1000, 3000]",
        ));
    }
    if boundaries.cached_ratio.is_empty()
        || !boundaries
            .cached_ratio
            .windows(2)
            .all(|pair| matches!(pair, [left, right] if left < right))
        || boundaries
            .cached_ratio
            .iter()
            .any(|boundary| !(0.0..=1.0).contains(boundary))
    {
        return Err(profile_error(
            "cached ratio boundaries must be strictly increasing values between 0 and 1",
        ));
    }
    Ok(())
}

fn validate_bucket_key(key: &BucketKey, boundaries: &BucketBoundaries) -> Result<(), io::Error> {
    if key.input > boundaries.input_tokens.len()
        || key.output > boundaries.output_tokens.len()
        || key.cached_ratio > boundaries.cached_ratio.len()
        || key.reasoning_effort.trim().is_empty()
    {
        return Err(profile_error(
            "workload bucket is outside frozen boundaries",
        ));
    }
    Ok(())
}

fn validate_latency(
    first_event_p50_ms: f64,
    first_event_p90_ms: Option<f64>,
    complete_p50_ms: f64,
    complete_p90_ms: Option<f64>,
) -> Result<(), io::Error> {
    if !first_event_p50_ms.is_finite()
        || first_event_p50_ms <= 0.0
        || !complete_p50_ms.is_finite()
        || complete_p50_ms <= 0.0
        || first_event_p90_ms.is_some_and(|value| !value.is_finite() || value < first_event_p50_ms)
        || complete_p90_ms.is_some_and(|value| !value.is_finite() || value < complete_p50_ms)
    {
        return Err(profile_error("workload latency quantiles are invalid"));
    }
    Ok(())
}

const fn leap_year(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn date_ordinal(value: &str) -> Result<i64, io::Error> {
    let mut parts = value.split('-');
    let year = parts
        .next()
        .ok_or_else(|| profile_error("date is missing year"))?
        .parse::<i64>()
        .map_err(|error| profile_error(error.to_string()))?;
    let month = parts
        .next()
        .ok_or_else(|| profile_error("date is missing month"))?
        .parse::<u32>()
        .map_err(|error| profile_error(error.to_string()))?;
    let day = parts
        .next()
        .ok_or_else(|| profile_error("date is missing day"))?
        .parse::<u32>()
        .map_err(|error| profile_error(error.to_string()))?;
    if value.len() != 10 || parts.next().is_some() || year < 1 {
        return Err(profile_error("date must use YYYY-MM-DD"));
    }
    let days_in_month = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap_year(year) => 29,
        2 => 28,
        _ => return Err(profile_error("date month is invalid")),
    };
    if day == 0 || day > days_in_month {
        return Err(profile_error("date day is invalid"));
    }
    let days_before_month = match month {
        1 => 0,
        2 => 31,
        3 => 59,
        4 => 90,
        5 => 120,
        6 => 151,
        7 => 181,
        8 => 212,
        9 => 243,
        10 => 273,
        11 => 304,
        12 => 334,
        _ => return Err(profile_error("date month is invalid")),
    };
    let prior_year = year - 1;
    Ok(prior_year * 365 + prior_year / 4 - prior_year / 100
        + prior_year / 400
        + i64::from(days_before_month)
        + i64::from(day)
        + i64::from(leap_year(year) && month > 2))
}

fn validate_profile(profile: &WorkloadProfile) -> Result<(), io::Error> {
    if profile.schema_version != PROFILE_SCHEMA_VERSION {
        return Err(profile_error("unsupported workload profile schema_version"));
    }
    if [
        profile.timezone.as_str(),
        profile.measurement_date.as_str(),
        profile.anchor_date.as_str(),
        profile.filters.model.as_str(),
        profile.filters.channel.as_str(),
        profile.filters.same_window.as_str(),
    ]
    .iter()
    .any(|value| value.trim().is_empty())
        || profile.filters.endpoint != "/v1/responses"
        || !profile.filters.stream
    {
        return Err(profile_error("workload profile filters are incomplete"));
    }
    let measurement_day = date_ordinal(&profile.measurement_date)?;
    let anchor_day = date_ordinal(&profile.anchor_date)?;
    if measurement_day.checked_sub(anchor_day) != Some(15) {
        return Err(profile_error(
            "anchor_date must be measurement_date minus 15 days",
        ));
    }
    validate_boundaries(&profile.bucket_boundaries)?;
    if profile.old_constants.baseline_first_token_ms == 0
        || profile.old_constants.baseline_complete_ms == 0
    {
        return Err(profile_error("old baseline constants must be positive"));
    }
    if profile.current.is_empty() {
        return Err(profile_error("workload profile has no current buckets"));
    }
    let mut current_keys = HashSet::new();
    for bucket in &profile.current {
        validate_bucket_key(&bucket.bucket, &profile.bucket_boundaries)?;
        if bucket.count == 0
            || bucket.http_requests.checked_add(bucket.websocket_requests) != Some(bucket.count)
            || bucket.raw_bytes_p90 < bucket.raw_bytes_p50
            || bucket.sent_bytes_p90 < bucket.sent_bytes_p50
            || !current_keys.insert(bucket.bucket.clone())
        {
            return Err(profile_error(
                "current workload bucket is invalid or duplicated",
            ));
        }
        validate_latency(
            bucket.first_event_p50_ms,
            Some(bucket.first_event_p90_ms),
            bucket.complete_p50_ms,
            Some(bucket.complete_p90_ms),
        )?;
    }
    validate_history(profile)
}

fn validate_history(profile: &WorkloadProfile) -> Result<(), io::Error> {
    if profile.history.len() < MIN_HISTORY_DATES {
        return Err(profile_error("fewer than three historical dates"));
    }
    if profile.history.len() < TARGET_HISTORY_DATES
        && profile.missing_dates.len() < TARGET_HISTORY_DATES - profile.history.len()
    {
        return Err(profile_error(
            "missing historical dates require reason codes",
        ));
    }
    let mut dates = HashSet::new();
    for day in &profile.history {
        date_ordinal(&day.date)?;
        if !dates.insert(day.date.as_str()) {
            return Err(profile_error("historical date is empty or duplicated"));
        }
        for buckets in [&day.same_window, &day.full_day] {
            let mut keys = HashSet::new();
            for bucket in buckets {
                validate_bucket_key(&bucket.bucket, &profile.bucket_boundaries)?;
                if bucket.count == 0 || !keys.insert(bucket.bucket.clone()) {
                    return Err(profile_error("historical bucket is invalid or duplicated"));
                }
                validate_latency(
                    bucket.first_event_p50_ms,
                    bucket.first_event_p90_ms,
                    bucket.complete_p50_ms,
                    bucket.complete_p90_ms,
                )?;
            }
        }
    }
    if !dates.contains(profile.anchor_date.as_str()) {
        return Err(profile_error("D-15 anchor date is missing from history"));
    }
    let mut missing_dates = HashSet::new();
    for missing in &profile.missing_dates {
        date_ordinal(&missing.date)?;
        if dates.contains(missing.date.as_str()) || !missing_dates.insert(missing.date.as_str()) {
            return Err(profile_error(
                "missing historical date is invalid or duplicated",
            ));
        }
        match missing.reason {
            MissingReason::InsufficientSamples
            | MissingReason::TurboPresent
            | MissingReason::FilterMismatch
            | MissingReason::DataUnavailable => {}
        }
    }
    Ok(())
}

fn current_total(current: &[CurrentBucket]) -> Result<u64, io::Error> {
    current.iter().try_fold(0_u64, |total, bucket| {
        total
            .checked_add(bucket.count)
            .ok_or_else(|| profile_error("workload sample count overflow"))
    })
}

fn current_latency(current: &[CurrentBucket]) -> Result<LatencyCalibration, io::Error> {
    let total = current_total(current)?;
    let total = count_as_f64(total);
    let weighted = |value: fn(&CurrentBucket) -> f64| {
        current
            .iter()
            .map(|bucket| count_as_f64(bucket.count) * value(bucket))
            .sum::<f64>()
            / total
    };
    Ok(LatencyCalibration {
        first_event_p50_ms: weighted(|bucket| bucket.first_event_p50_ms),
        first_event_p90_ms: Some(weighted(|bucket| bucket.first_event_p90_ms)),
        complete_p50_ms: weighted(|bucket| bucket.complete_p50_ms),
        complete_p90_ms: Some(weighted(|bucket| bucket.complete_p90_ms)),
    })
}

fn calibrate_scope(
    current: &[CurrentBucket],
    historical: &[HistoricalBucket],
) -> Result<ScopeCalibration, io::Error> {
    let total_current = current_total(current)?;
    let mut covered_current_count = 0_u64;
    let mut sample_count = 0_u64;
    let mut first_p50 = 0.0;
    let mut complete_p50 = 0.0;
    let mut first_p90 = 0.0;
    let mut complete_p90 = 0.0;
    let mut p90_available = true;
    let mut buckets = Vec::with_capacity(current.len());
    for current_bucket in current {
        let historical_bucket = historical
            .iter()
            .find(|candidate| candidate.bucket == current_bucket.bucket);
        buckets.push(BucketEvidence {
            bucket: current_bucket.bucket.clone(),
            current_samples: current_bucket.count,
            historical_samples: historical_bucket.map_or(0, |bucket| bucket.count),
            profile_coverage_pct: historical_bucket.map_or(0.0, |_| {
                count_as_f64(current_bucket.count) / count_as_f64(total_current) * 100.0
            }),
        });
        let Some(historical_bucket) = historical_bucket else {
            continue;
        };
        covered_current_count = covered_current_count
            .checked_add(current_bucket.count)
            .ok_or_else(|| profile_error("covered workload count overflow"))?;
        sample_count = sample_count
            .checked_add(historical_bucket.count)
            .ok_or_else(|| profile_error("historical sample count overflow"))?;
        let weight = count_as_f64(current_bucket.count);
        first_p50 = weight.mul_add(historical_bucket.first_event_p50_ms, first_p50);
        complete_p50 = weight.mul_add(historical_bucket.complete_p50_ms, complete_p50);
        match (
            historical_bucket.count >= MIN_P90_BUCKET_SAMPLES,
            historical_bucket.first_event_p90_ms,
            historical_bucket.complete_p90_ms,
        ) {
            (true, Some(first), Some(complete)) => {
                first_p90 = weight.mul_add(first, first_p90);
                complete_p90 = weight.mul_add(complete, complete_p90);
            }
            _ => p90_available = false,
        }
    }
    let coverage_pct = count_as_f64(covered_current_count) / count_as_f64(total_current) * 100.0;
    if coverage_pct + f64::EPSILON < MIN_MATCH_COVERAGE_PCT {
        return Err(profile_error(format!(
            "historical match coverage {coverage_pct:.1}% is below 70%"
        )));
    }
    let covered = count_as_f64(covered_current_count);
    Ok(ScopeCalibration {
        sample_count,
        covered_current_count,
        coverage_pct,
        buckets,
        latency: LatencyCalibration {
            first_event_p50_ms: first_p50 / covered,
            first_event_p90_ms: p90_available.then_some(first_p90 / covered),
            complete_p50_ms: complete_p50 / covered,
            complete_p90_ms: p90_available.then_some(complete_p90 / covered),
        },
    })
}

fn baseline(
    days: &[DayCalibration],
    scope: impl Fn(&DayCalibration) -> &ScopeCalibration,
) -> Result<LatencyCalibration, io::Error> {
    let first_p90 = days
        .iter()
        .map(|day| scope(day).latency.first_event_p90_ms)
        .collect::<Option<Vec<_>>>()
        .and_then(median);
    let complete_p90 = days
        .iter()
        .map(|day| scope(day).latency.complete_p90_ms)
        .collect::<Option<Vec<_>>>()
        .and_then(median);
    Ok(LatencyCalibration {
        first_event_p50_ms: median(days.iter().map(|day| scope(day).latency.first_event_p50_ms))
            .ok_or_else(|| profile_error("historical baseline has no dates"))?,
        first_event_p90_ms: first_p90,
        complete_p50_ms: median(days.iter().map(|day| scope(day).latency.complete_p50_ms))
            .ok_or_else(|| profile_error("historical baseline has no dates"))?,
        complete_p90_ms: complete_p90,
    })
}

fn calibrate_profile_json(bytes: &[u8]) -> Result<ProfileCalibration, io::Error> {
    let profile = serde_json::from_slice::<WorkloadProfile>(bytes)
        .map_err(|error| profile_error(error.to_string()))?;
    validate_profile(&profile)?;
    let days = profile
        .history
        .iter()
        .map(|day| {
            Ok(DayCalibration {
                date: day.date.clone(),
                same_window: calibrate_scope(&profile.current, &day.same_window)?,
                full_day: calibrate_scope(&profile.current, &day.full_day)?,
            })
        })
        .collect::<Result<Vec<_>, io::Error>>()?;
    Ok(ProfileCalibration {
        profile_fingerprint: format!("fnv1a64:{:016x}", workload_fingerprint(bytes)),
        current: current_latency(&profile.current)?,
        baseline_same_window: baseline(&days, |day| &day.same_window)?,
        baseline_full_day: baseline(&days, |day| &day.full_day)?,
        days,
        profile,
    })
}

fn continuation_case<'a>(
    cases: &'a [BenchmarkCase],
    path: &str,
) -> Result<&'a BenchmarkCase, io::Error> {
    cases
        .iter()
        .find(|case| {
            case.path == path
                && case
                    .samples
                    .first()
                    .is_some_and(|sample| sample.logical_requests > 1)
        })
        .ok_or_else(|| profile_error(format!("missing continuation benchmark path {path}")))
}

fn mechanism_evidence(cases: &[BenchmarkCase]) -> Result<(MechanismEvidence, f64, f64), io::Error> {
    let core_cases = [DIRECT_PATH, HTTP_PATH, WEBSOCKET_PATH, HYBRID_PATH]
        .map(|path| continuation_case(cases, path))
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?;
    let scenario = core_cases
        .first()
        .ok_or_else(|| profile_error("benchmark has no core paths"))?
        .scenario;
    for case in &core_cases {
        if case.scenario != scenario {
            return Err(profile_error(
                "core continuation paths do not share one scenario",
            ));
        }
        case_report(case)?;
        super::stability::validate_sample_count(case)?;
    }
    let hybrid = continuation_case(cases, HYBRID_PATH)?;
    if hybrid
        .samples
        .iter()
        .filter(|sample| sample.retries == 0)
        .any(|sample| {
            sample.websocket_reconnects != 0
                || sample.round_transports.is_empty()
                || sample
                    .round_transports
                    .iter()
                    .any(|transport| *transport != super::RoundTransport::WebSocket)
        })
    {
        return Err(profile_error(
            "Hybrid continuation constants require already-warmed WebSocket rounds and no reconnects",
        ));
    }
    let http = case_report(continuation_case(cases, HTTP_PATH)?)?;
    let hybrid = case_report(hybrid)?;
    let http_first_event_ms = http
        .http_ttft
        .ok_or_else(|| profile_error("Turbo HTTP TTFT is missing"))?
        .median;
    let websocket_first_event_ms = hybrid
        .websocket_ttft
        .ok_or_else(|| profile_error("Hybrid WS TTFT is missing"))?
        .median;
    let http_complete_ms = http
        .http_complete
        .ok_or_else(|| profile_error("Turbo HTTP complete is missing"))?
        .median;
    let websocket_complete_ms = hybrid
        .websocket_complete
        .ok_or_else(|| profile_error("Hybrid WS complete is missing"))?
        .median;
    let paired_first = super::stability::paired_savings(
        continuation_case(cases, HTTP_PATH)?,
        continuation_case(cases, HYBRID_PATH)?,
        super::stability::PairedMetric::FirstEvent,
    )?;
    if !paired_first.qualifies {
        return Err(profile_error(format!(
            "paired first-event savings are not candidate-ready: both sample halves must remain positive, median={:.1} ms, half-sample drift={:?}%",
            paired_first.median_ms, paired_first.drift_pct,
        )));
    }
    let paired_complete = super::stability::paired_savings(
        continuation_case(cases, HTTP_PATH)?,
        continuation_case(cases, HYBRID_PATH)?,
        super::stability::PairedMetric::Complete,
    )?;
    let complete_saved_ms = if paired_complete.qualifies {
        paired_complete.median_ms
    } else {
        0.0
    };
    Ok((
        MechanismEvidence {
            http_first_event_ms,
            websocket_first_event_ms,
            http_complete_ms,
            websocket_complete_ms,
            paired_first_event_saved_ms: paired_first.median_ms,
            paired_first_event_drift_pct: paired_first.drift_pct.ok_or_else(|| {
                profile_error("paired first-event savings require a positive first-half median")
            })?,
            paired_complete_saved_ms: paired_complete.median_ms,
            paired_complete_drift_pct: paired_complete.drift_pct,
            complete_constant_qualified: paired_complete.qualifies,
        },
        paired_first.median_ms,
        complete_saved_ms,
    ))
}

fn rounded_milliseconds(value: f64) -> Result<u64, io::Error> {
    if !value.is_finite() || value < 0.0 {
        return Err(profile_error(
            "candidate latency is not a finite positive value",
        ));
    }
    u64::try_from(Duration::from_secs_f64((value + 0.5) / 1_000.0).as_millis())
        .map_err(|error| profile_error(error.to_string()))
}

fn constant_change(old_ms: u64, candidate_ms: u64) -> Result<ConstantChange, io::Error> {
    let old = i64::try_from(old_ms).map_err(|error| profile_error(error.to_string()))?;
    let candidate =
        i64::try_from(candidate_ms).map_err(|error| profile_error(error.to_string()))?;
    let absolute_ms = candidate
        .checked_sub(old)
        .ok_or_else(|| profile_error("constant change overflow"))?;
    let absolute_ms_f64 = count_as_f64(absolute_ms.unsigned_abs());
    Ok(ConstantChange {
        old_ms,
        candidate_ms,
        absolute_ms,
        percent: if absolute_ms < 0 {
            -absolute_ms_f64
        } else {
            absolute_ms_f64
        } / count_as_f64(old_ms)
            * 100.0,
    })
}

fn constant_changes(
    old: SpeedConstants,
    candidate: SpeedConstants,
) -> Result<ConstantChanges, io::Error> {
    Ok(ConstantChanges {
        baseline_first_token_ms: constant_change(
            old.baseline_first_token_ms,
            candidate.baseline_first_token_ms,
        )?,
        baseline_complete_ms: constant_change(
            old.baseline_complete_ms,
            candidate.baseline_complete_ms,
        )?,
        websocket_first_token_saved_ms: constant_change(
            old.websocket_first_token_saved_ms,
            candidate.websocket_first_token_saved_ms,
        )?,
        websocket_complete_saved_ms: constant_change(
            old.websocket_complete_saved_ms,
            candidate.websocket_complete_saved_ms,
        )?,
    })
}

fn historical_gain_pct(baseline_ms: f64, current_ms: f64) -> f64 {
    (baseline_ms - current_ms) / baseline_ms * 100.0
}

fn compression_sensitivity(
    profile: &WorkloadProfile,
    constants: SpeedConstants,
) -> Result<Vec<CompressionSensitivity>, io::Error> {
    let total = current_total(&profile.current)?;
    let websocket_requests = profile.current.iter().try_fold(0_u64, |count, bucket| {
        count
            .checked_add(bucket.websocket_requests)
            .ok_or_else(|| profile_error("WebSocket sample count overflow"))
    })?;
    let saved_bytes = profile
        .current
        .iter()
        .map(|bucket| {
            count_as_f64(bucket.count)
                * (count_as_f64(bucket.raw_bytes_p50) - count_as_f64(bucket.sent_bytes_p50))
        })
        .sum::<f64>()
        / count_as_f64(total);
    let websocket_weight = count_as_f64(websocket_requests) / count_as_f64(total);
    Ok([5.0, 10.0, 20.0]
        .into_iter()
        .map(|uplink_mbps| {
            let upload_saved_ms = saved_bytes * 8.0 / (uplink_mbps * 1_000_000.0) * 1_000.0;
            CompressionSensitivity {
                uplink_mbps,
                upload_saved_ms,
                estimated_first_event_gain_pct: websocket_weight.mul_add(
                    count_as_f64(constants.websocket_first_token_saved_ms),
                    upload_saved_ms,
                ) / count_as_f64(constants.baseline_first_token_ms)
                    * 100.0,
                estimated_complete_gain_pct: websocket_weight.mul_add(
                    count_as_f64(constants.websocket_complete_saved_ms),
                    upload_saved_ms,
                ) / count_as_f64(constants.baseline_complete_ms)
                    * 100.0,
            }
        })
        .collect())
}

fn workload_profile_evidence(
    calibration: &ProfileCalibration,
) -> Result<WorkloadProfileEvidence, io::Error> {
    let current_samples = current_total(&calibration.profile.current)?;
    let (http_requests, websocket_requests) = calibration.profile.current.iter().try_fold(
        (0_u64, 0_u64),
        |(http, websocket), bucket| {
            Ok::<_, io::Error>((
                http.checked_add(bucket.http_requests)
                    .ok_or_else(|| profile_error("HTTP sample count overflow"))?,
                websocket
                    .checked_add(bucket.websocket_requests)
                    .ok_or_else(|| profile_error("WebSocket sample count overflow"))?,
            ))
        },
    )?;
    Ok(WorkloadProfileEvidence {
        fingerprint: calibration.profile_fingerprint.clone(),
        timezone: calibration.profile.timezone.clone(),
        measurement_date: calibration.profile.measurement_date.clone(),
        anchor_date: calibration.profile.anchor_date.clone(),
        history_dates: calibration
            .days
            .iter()
            .map(|day| day.date.clone())
            .collect(),
        missing_dates: calibration
            .profile
            .missing_dates
            .iter()
            .map(|missing| MissingDateEvidence {
                date: missing.date.clone(),
                reason: missing.reason.label(),
            })
            .collect(),
        filters: ProfileFilterEvidence {
            model: calibration.profile.filters.model.clone(),
            channel: calibration.profile.filters.channel.clone(),
            endpoint: calibration.profile.filters.endpoint.clone(),
            stream: calibration.profile.filters.stream,
            same_window: calibration.profile.filters.same_window.clone(),
        },
        bucket_boundaries: BucketBoundaryEvidence {
            input_tokens: calibration.profile.bucket_boundaries.input_tokens.clone(),
            output_tokens: calibration.profile.bucket_boundaries.output_tokens.clone(),
            cached_ratio: calibration.profile.bucket_boundaries.cached_ratio.clone(),
        },
        current_samples,
        http_requests,
        websocket_requests,
    })
}

fn historical_calibration_evidence(
    calibration: &ProfileCalibration,
) -> HistoricalCalibrationEvidence {
    HistoricalCalibrationEvidence {
        current: calibration.current.into(),
        baseline_same_window: calibration.baseline_same_window.into(),
        baseline_full_day: calibration.baseline_full_day.into(),
        days: calibration
            .days
            .iter()
            .map(|day| DayEvidence {
                date: day.date.clone(),
                same_window: (&day.same_window).into(),
                full_day: (&day.full_day).into(),
            })
            .collect(),
    }
}

fn benchmark_evidence(
    settings: &BenchmarkSettings,
    cases: &[BenchmarkCase],
) -> Result<BenchmarkEvidence, io::Error> {
    let core_paths = [DIRECT_PATH, HTTP_PATH, WEBSOCKET_PATH, HYBRID_PATH]
        .into_iter()
        .map(|path| {
            let case = continuation_case(cases, path)?;
            let valid_samples = case
                .samples
                .iter()
                .filter(|sample| sample.retries == 0)
                .count();
            Ok(PathEvidence {
                scenario: case.scenario,
                path: case.path,
                valid_samples,
                recovered_samples: case.samples.len().saturating_sub(valid_samples),
                retries: case.samples.iter().map(|sample| sample.retries).sum(),
            })
        })
        .collect::<Result<Vec<_>, io::Error>>()?;
    Ok(BenchmarkEvidence {
        upstream: settings.upstream.clone(),
        model: settings.model.clone(),
        runs: settings.runs,
        warmups: settings.warmups,
        timeout_seconds: settings.timeout.as_secs(),
        workload_source: settings.workload_source.label(),
        workload_fingerprint: format!(
            "fnv1a64:{:016x}",
            workload_fingerprint(settings.prompt.as_bytes())
        ),
        workload_bytes: settings.prompt.len(),
        core_paths,
    })
}

fn candidate_summary(
    profile_bytes: &[u8],
    settings: &BenchmarkSettings,
    cases: &[BenchmarkCase],
) -> Result<CandidateSummary, io::Error> {
    let calibration = calibrate_profile_json(profile_bytes)?;
    if calibration.profile.filters.model != settings.model {
        return Err(profile_error(
            "workload profile model does not match live benchmark model",
        ));
    }
    let (mechanism, first_saved_ms, complete_saved_ms) = mechanism_evidence(cases)?;
    if calibration.baseline_same_window.first_event_p50_ms <= calibration.current.first_event_p50_ms
        || calibration.baseline_same_window.complete_p50_ms <= calibration.current.complete_p50_ms
    {
        return Err(profile_error(
            "historical calibration direction conflicts with mechanism benchmark",
        ));
    }
    let old_constants = calibration.profile.old_constants;
    let measured_constants = SpeedConstants {
        baseline_first_token_ms: rounded_milliseconds(
            calibration.baseline_same_window.first_event_p50_ms,
        )?,
        baseline_complete_ms: rounded_milliseconds(
            calibration.baseline_same_window.complete_p50_ms,
        )?,
        websocket_first_token_saved_ms: rounded_milliseconds(first_saved_ms)?,
        websocket_complete_saved_ms: rounded_milliseconds(complete_saved_ms)?,
    };
    let first_gain_increased = u128::from(measured_constants.websocket_first_token_saved_ms)
        * u128::from(old_constants.baseline_first_token_ms)
        > u128::from(old_constants.websocket_first_token_saved_ms)
            * u128::from(measured_constants.baseline_first_token_ms);
    let complete_gain_increased = mechanism.complete_constant_qualified
        && u128::from(measured_constants.websocket_complete_saved_ms)
            * u128::from(old_constants.baseline_complete_ms)
            > u128::from(old_constants.websocket_complete_saved_ms)
                * u128::from(measured_constants.baseline_complete_ms);
    let candidate_constants = SpeedConstants {
        baseline_first_token_ms: if first_gain_increased {
            measured_constants.baseline_first_token_ms
        } else {
            old_constants.baseline_first_token_ms
        },
        baseline_complete_ms: if complete_gain_increased {
            measured_constants.baseline_complete_ms
        } else {
            old_constants.baseline_complete_ms
        },
        websocket_first_token_saved_ms: if first_gain_increased {
            measured_constants.websocket_first_token_saved_ms
        } else {
            old_constants.websocket_first_token_saved_ms
        },
        websocket_complete_saved_ms: if complete_gain_increased {
            measured_constants.websocket_complete_saved_ms
        } else {
            old_constants.websocket_complete_saved_ms
        },
    };
    let workload_profile = workload_profile_evidence(&calibration)?;
    let benchmark = benchmark_evidence(settings, cases)?;
    let historical_calibration = historical_calibration_evidence(&calibration);
    Ok(CandidateSummary {
        schema_version: PROFILE_SCHEMA_VERSION,
        status: "candidate_not_applied",
        workload_profile,
        benchmark,
        historical_calibration,
        old_constants,
        candidate_constants,
        changes: constant_changes(old_constants, candidate_constants)?,
        historical_observation: HistoricalObservation {
            first_event_gain_pct: historical_gain_pct(
                calibration.baseline_same_window.first_event_p50_ms,
                calibration.current.first_event_p50_ms,
            ),
            complete_gain_pct: historical_gain_pct(
                calibration.baseline_same_window.complete_p50_ms,
                calibration.current.complete_p50_ms,
            ),
        },
        compression_sensitivity: compression_sensitivity(
            &calibration.profile,
            candidate_constants,
        )?,
        mechanism,
    })
}

fn candidate_json(summary: &CandidateSummary) -> Result<Vec<u8>, io::Error> {
    serde_json::to_vec_pretty(summary).map_err(|error| profile_error(error.to_string()))
}

fn write_constant_change(
    output: &mut impl Write,
    name: &str,
    change: ConstantChange,
) -> io::Result<()> {
    writeln!(
        output,
        "{name} | {} → {} | {:+} ms | {:+.1}%",
        change.old_ms, change.candidate_ms, change.absolute_ms, change.percent
    )
}

fn write_bucket_evidence(
    output: &mut impl Write,
    label: &str,
    scope: &ScopeEvidence,
) -> io::Result<()> {
    writeln!(output, "  {label}逐桶：")?;
    for bucket in &scope.buckets {
        writeln!(
            output,
            "    bucket[input={}, output={}, cached_ratio={}, reasoning_effort={}] current_n={} history_n={} coverage={:.1}%",
            bucket.bucket.input,
            bucket.bucket.output,
            bucket.bucket.cached_ratio,
            bucket.bucket.reasoning_effort,
            bucket.current_samples,
            bucket.historical_samples,
            bucket.profile_coverage_pct,
        )?;
    }
    Ok(())
}

fn write_mechanism_evidence(
    output: &mut impl Write,
    summary: &CandidateSummary,
) -> Result<(), io::Error> {
    writeln!(
        output,
        "机制层：HTTP→WS 同轮配对首事件节省 {:.1} ms（前后半漂移 {:.1}%）；只使用 Hybrid 的纯 WS warm rounds。",
        summary.mechanism.paired_first_event_saved_ms,
        summary.mechanism.paired_first_event_drift_pct,
    )?;
    writeln!(
        output,
        "complete：HTTP/WS 聚合中位数 {:.1}/{:.1} ms，同轮配对差 {:.1} ms，漂移 {:?}%，固定收益资格={}；不合格或收益比例未提高时保留旧 complete 常量组。",
        summary.mechanism.http_complete_ms,
        summary.mechanism.websocket_complete_ms,
        summary.mechanism.paired_complete_saved_ms,
        summary.mechanism.paired_complete_drift_pct,
        summary.mechanism.complete_constant_qualified,
    )
}

fn write_candidate_report(
    output: &mut impl Write,
    summary: &CandidateSummary,
) -> Result<(), io::Error> {
    writeln!(output, "\n真实使用校准候选（未应用）")?;
    writeln!(
        output,
        "profile={}，测量日={}，D-15 锚点={}，历史日期={}，当前样本={}（HTTP={} / WS={}）",
        summary.workload_profile.fingerprint,
        summary.workload_profile.measurement_date,
        summary.workload_profile.anchor_date,
        summary.workload_profile.history_dates.join(","),
        summary.workload_profile.current_samples,
        summary.workload_profile.http_requests,
        summary.workload_profile.websocket_requests,
    )?;
    writeln!(
        output,
        "匹配口径：模型={}，渠道={}，接口={}，stream={}，同窗={}",
        summary.workload_profile.filters.model,
        summary.workload_profile.filters.channel,
        summary.workload_profile.filters.endpoint,
        summary.workload_profile.filters.stream,
        summary.workload_profile.filters.same_window,
    )?;
    writeln!(output, "常量 | 旧值 → 候选值 | 绝对变化 | 相对变化")?;
    write_constant_change(
        output,
        "baselineFirstTokenMs",
        summary.changes.baseline_first_token_ms,
    )?;
    write_constant_change(
        output,
        "baselineCompleteMs",
        summary.changes.baseline_complete_ms,
    )?;
    write_constant_change(
        output,
        "websocketFirstTokenSavedMs",
        summary.changes.websocket_first_token_saved_ms,
    )?;
    write_constant_change(
        output,
        "websocketCompleteSavedMs",
        summary.changes.websocket_complete_saved_ms,
    )?;
    write_mechanism_evidence(output, summary)?;
    writeln!(
        output,
        "历史观测：首事件 {:+.1}%，complete {:+.1}%；历史 before/after 仅作交叉验证，不归因于 Turbo。",
        summary.historical_observation.first_event_gain_pct,
        summary.historical_observation.complete_gain_pct,
    )?;
    for day in &summary.historical_calibration.days {
        writeln!(
            output,
            "{}：同窗 P50={:.1}/{:.1} ms P90={:?}/{:?} n={} coverage={:.1}%；全天 P50={:.1}/{:.1} ms P90={:?}/{:?} n={} coverage={:.1}%",
            day.date,
            day.same_window.latency.first_event_p50_ms,
            day.same_window.latency.complete_p50_ms,
            day.same_window.latency.first_event_p90_ms,
            day.same_window.latency.complete_p90_ms,
            day.same_window.sample_count,
            day.same_window.coverage_pct,
            day.full_day.latency.first_event_p50_ms,
            day.full_day.latency.complete_p50_ms,
            day.full_day.latency.first_event_p90_ms,
            day.full_day.latency.complete_p90_ms,
            day.full_day.sample_count,
            day.full_day.coverage_pct,
        )?;
        write_bucket_evidence(output, "同窗", &day.same_window)?;
        write_bucket_evidence(output, "全天", &day.full_day)?;
    }
    writeln!(
        output,
        "带宽 | 上传节省 | 首事件估算收益 | complete 估算收益"
    )?;
    for sensitivity in &summary.compression_sensitivity {
        writeln!(
            output,
            "{:.0} Mbps | {:.1} ms | {:.1}% | {:.1}%",
            sensitivity.uplink_mbps,
            sensitivity.upload_saved_ms,
            sensitivity.estimated_first_event_gain_pct,
            sensitivity.estimated_complete_gain_pct,
        )?;
    }
    writeln!(
        output,
        "候选值仅写入报告，不会自动修改 Turbo 产品常量；模型、服务端负载和时段波动仍属不可归因项。"
    )
}

pub(super) fn generate_candidate_artifacts(
    human_output: &mut impl Write,
    profile_path: &Path,
    candidate_output_path: &Path,
    settings: &BenchmarkSettings,
    cases: &[BenchmarkCase],
) -> Result<(), io::Error> {
    if profile_path == candidate_output_path {
        return Err(profile_error(
            "candidate output path must differ from workload profile path",
        ));
    }
    let profile = fs::read(profile_path)?;
    let summary = candidate_summary(&profile, settings, cases)?;
    let machine = candidate_json(&summary)?;
    write_candidate_report(human_output, &summary)?;
    fs::write(candidate_output_path, machine)
}

#[cfg(test)]
mod tests {
    use std::io;

    use serde_json::{Value, json};

    use super::calibrate_profile_json;

    fn profile() -> Result<Value, io::Error> {
        serde_json::from_str(include_str!(
            "../../../docs/verification/2026-08-10-workload-profile.example.json"
        ))
        .map_err(io::Error::other)
    }

    #[test]
    fn gives_each_historical_date_equal_weight() -> Result<(), io::Error> {
        let bytes = serde_json::to_vec(&profile()?).map_err(io::Error::other)?;

        let calibration = calibrate_profile_json(&bytes)?;

        assert_eq!(calibration.days.len(), 3);
        assert!((calibration.baseline_same_window.first_event_p50_ms - 2_000.0).abs() < 0.001);
        assert!((calibration.baseline_same_window.complete_p50_ms - 4_000.0).abs() < 0.001);
        Ok(())
    }

    #[test]
    fn rejects_profiles_below_seventy_percent_match_coverage() -> Result<(), io::Error> {
        let mut profile = profile()?;
        let same_window = profile
            .pointer_mut("/history/0/same_window")
            .ok_or_else(|| io::Error::other("missing same-window fixture"))?;
        *same_window = json!([]);
        let bytes = serde_json::to_vec(&profile).map_err(io::Error::other)?;

        let error = calibrate_profile_json(&bytes)
            .expect_err("low coverage must refuse calibration")
            .to_string();

        assert!(error.contains("coverage 0.0% is below 70%"));
        Ok(())
    }

    #[test]
    fn omits_historical_p90_when_a_matched_bucket_is_too_small() -> Result<(), io::Error> {
        let mut profile = profile()?;
        let count = profile
            .pointer_mut("/history/0/same_window/0/count")
            .ok_or_else(|| io::Error::other("missing history count fixture"))?;
        *count = json!(11);
        let bytes = serde_json::to_vec(&profile).map_err(io::Error::other)?;

        let calibration = calibrate_profile_json(&bytes)?;

        assert!(
            calibration
                .baseline_same_window
                .first_event_p90_ms
                .is_none()
        );
        assert!(calibration.baseline_same_window.complete_p90_ms.is_none());
        Ok(())
    }

    #[test]
    fn rejects_sensitive_or_unrecognized_profile_fields() -> Result<(), io::Error> {
        let mut profile = profile()?;
        profile
            .as_object_mut()
            .ok_or_else(|| io::Error::other("profile fixture is not an object"))?
            .insert("user_id".to_owned(), json!("HTH"));
        let bytes = serde_json::to_vec(&profile).map_err(io::Error::other)?;

        let error = calibrate_profile_json(&bytes)
            .expect_err("sensitive extra fields must be rejected")
            .to_string();

        assert!(error.contains("unknown field `user_id`"));
        Ok(())
    }

    #[test]
    fn rejects_an_anchor_that_is_not_measurement_day_minus_fifteen() -> Result<(), io::Error> {
        let mut profile = profile()?;
        *profile
            .pointer_mut("/anchor_date")
            .ok_or_else(|| io::Error::other("missing anchor fixture"))? = json!("2026-07-25");
        *profile
            .pointer_mut("/history/0/date")
            .ok_or_else(|| io::Error::other("missing historical date fixture"))? =
            json!("2026-07-25");
        let bytes = serde_json::to_vec(&profile).map_err(io::Error::other)?;

        let error = calibrate_profile_json(&bytes)
            .expect_err("non-D-15 anchor must be rejected")
            .to_string();

        assert!(error.contains("anchor_date must be measurement_date minus 15 days"));
        Ok(())
    }

    #[test]
    fn matches_input_output_cache_and_reasoning_buckets_exactly() -> Result<(), io::Error> {
        let mut profile = profile()?;
        *profile
            .pointer_mut("/current/0/count")
            .ok_or_else(|| io::Error::other("missing current count fixture"))? = json!(70);
        *profile
            .pointer_mut("/current/0/http_requests")
            .ok_or_else(|| io::Error::other("missing current HTTP fixture"))? = json!(7);
        *profile
            .pointer_mut("/current/0/websocket_requests")
            .ok_or_else(|| io::Error::other("missing current WS fixture"))? = json!(63);
        profile
            .pointer_mut("/current")
            .and_then(serde_json::Value::as_array_mut)
            .ok_or_else(|| io::Error::other("missing current buckets fixture"))?
            .push(json!({
                "bucket": {
                    "input": 1,
                    "output": 1,
                    "cached_ratio": 1,
                    "reasoning_effort": "medium"
                },
                "count": 30,
                "http_requests": 3,
                "websocket_requests": 27,
                "raw_bytes_p50": 50_000,
                "raw_bytes_p90": 80_000,
                "sent_bytes_p50": 20_000,
                "sent_bytes_p90": 30_000,
                "first_event_p50_ms": 300.0,
                "first_event_p90_ms": 500.0,
                "complete_p50_ms": 3_000.0,
                "complete_p90_ms": 5_000.0
            }));
        let bytes = serde_json::to_vec(&profile).map_err(io::Error::other)?;

        let calibration = calibrate_profile_json(&bytes)?;
        let first_day = calibration
            .days
            .first()
            .ok_or_else(|| io::Error::other("missing historical calibration"))?;

        assert_eq!(first_day.same_window.covered_current_count, 70);
        assert!((first_day.same_window.coverage_pct - 70.0).abs() < 0.001);
        Ok(())
    }

    fn benchmark_cases(include_cold_hybrid_round: bool) -> Vec<super::super::BenchmarkCase> {
        use std::time::Duration;

        use super::super::{
            BenchmarkCase, DIRECT_PATH, HTTP_PATH, HYBRID_PATH, RoundTransport, Sample,
            WEBSOCKET_PATH,
        };

        [DIRECT_PATH, HTTP_PATH, WEBSOCKET_PATH, HYBRID_PATH]
            .into_iter()
            .map(|path| BenchmarkCase {
                scenario: "continuation",
                path,
                samples: (0..8)
                    .map(|_| {
                        let (round_transports, first_events, round_e2e) = match path {
                            HYBRID_PATH if include_cold_hybrid_round => (
                                vec![
                                    RoundTransport::Http,
                                    RoundTransport::WebSocket,
                                    RoundTransport::WebSocket,
                                    RoundTransport::WebSocket,
                                ],
                                vec![
                                    Duration::from_secs(9),
                                    Duration::from_millis(100),
                                    Duration::from_millis(100),
                                    Duration::from_millis(100),
                                ],
                                vec![
                                    Duration::from_secs(20),
                                    Duration::from_millis(100),
                                    Duration::from_millis(100),
                                    Duration::from_millis(100),
                                ],
                            ),
                            HYBRID_PATH | WEBSOCKET_PATH => (
                                vec![RoundTransport::WebSocket; 3],
                                vec![Duration::from_millis(100); 3],
                                vec![Duration::from_millis(100); 3],
                            ),
                            _ => (
                                vec![RoundTransport::Http; 3],
                                vec![Duration::from_millis(500); 3],
                                vec![Duration::from_secs(1); 3],
                            ),
                        };
                        let logical_requests = u64::try_from(round_transports.len()).unwrap_or(0);
                        let http_requests = u64::try_from(
                            round_transports
                                .iter()
                                .filter(|transport| **transport == RoundTransport::Http)
                                .count(),
                        )
                        .unwrap_or(0);
                        let websocket_messages = logical_requests.saturating_sub(http_requests);
                        Sample {
                            e2e: round_e2e.iter().sum(),
                            setup: Duration::from_millis(20),
                            raw_bytes: 100_000,
                            encoded_bytes: 25_000,
                            logical_requests,
                            application_messages: logical_requests,
                            http_requests,
                            websocket_messages,
                            response_events: logical_requests,
                            websocket_handshakes: u64::from(websocket_messages > 0),
                            warm_round_e2e: round_e2e.iter().skip(1).copied().collect(),
                            round_e2e,
                            first_events,
                            connection_lifetime: (websocket_messages > 0)
                                .then_some(Duration::from_secs(1)),
                            websocket_reconnects: 0,
                            messages_per_connection: (websocket_messages > 0)
                                .then_some(websocket_messages),
                            retries: 0,
                            round_transports,
                            compression_metrics: None,
                        }
                    })
                    .collect(),
            })
            .collect()
    }

    #[test]
    fn candidate_uses_only_hybrid_websocket_rounds_for_fixed_savings() -> Result<(), io::Error> {
        use std::time::Duration;

        use super::super::{BenchmarkSettings, settings::WorkloadSource};
        use super::candidate_summary;

        let bytes = serde_json::to_vec(&profile()?).map_err(io::Error::other)?;
        let settings = BenchmarkSettings {
            upstream: "https://api.ai-cove.com/v1".to_owned(),
            model: "gpt-5.6-luna".to_owned(),
            prompt: "fixed workload".to_owned(),
            workload_source: WorkloadSource::BuiltIn,
            runs: 8,
            warmups: 1,
            timeout: Duration::from_secs(180),
        };

        let three_rounds = candidate_summary(&bytes, &settings, &benchmark_cases(false))?;
        let cold_mixed = candidate_summary(&bytes, &settings, &benchmark_cases(true))
            .expect_err("cold Hybrid HTTP rounds must stay out of candidate constants")
            .to_string();

        assert_eq!(
            three_rounds.candidate_constants.baseline_first_token_ms,
            1_661
        );
        assert_eq!(three_rounds.candidate_constants.baseline_complete_ms, 4_000);
        assert_eq!(
            three_rounds
                .candidate_constants
                .websocket_first_token_saved_ms,
            469
        );
        assert_eq!(
            three_rounds.candidate_constants.websocket_complete_saved_ms,
            900
        );
        assert!(cold_mixed.contains("already-warmed WebSocket rounds"));
        Ok(())
    }

    #[test]
    fn candidate_ignores_unrelated_path_e2e_drift_when_paired_savings_are_stable()
    -> Result<(), io::Error> {
        use std::time::Duration;

        use super::super::{
            BenchmarkSettings, DIRECT_PATH, WEBSOCKET_PATH, settings::WorkloadSource,
        };
        use super::candidate_summary;

        let bytes = serde_json::to_vec(&profile()?).map_err(io::Error::other)?;
        let settings = BenchmarkSettings {
            upstream: "https://api.ai-cove.com/v1".to_owned(),
            model: "gpt-5.6-luna".to_owned(),
            prompt: "fixed workload".to_owned(),
            workload_source: WorkloadSource::BuiltIn,
            runs: 8,
            warmups: 1,
            timeout: Duration::from_secs(180),
        };
        let mut cases = benchmark_cases(false);
        for path in [DIRECT_PATH, WEBSOCKET_PATH] {
            let case = cases
                .iter_mut()
                .find(|case| case.path == path)
                .ok_or_else(|| io::Error::other("missing unrelated path fixture"))?;
            for (index, sample) in case.samples.iter_mut().enumerate() {
                sample.e2e = Duration::from_millis(if index < 4 { 1_000 } else { 2_000 });
            }
        }

        let summary = candidate_summary(&bytes, &settings, &cases)?;

        assert_eq!(
            summary.candidate_constants.websocket_first_token_saved_ms,
            469
        );
        Ok(())
    }

    #[test]
    fn candidate_keeps_old_complete_pair_when_paired_complete_is_not_positive()
    -> Result<(), io::Error> {
        use std::time::Duration;

        use super::super::{BenchmarkSettings, HYBRID_PATH, settings::WorkloadSource};
        use super::candidate_summary;

        let bytes = serde_json::to_vec(&profile()?).map_err(io::Error::other)?;
        let settings = BenchmarkSettings {
            upstream: "https://api.ai-cove.com/v1".to_owned(),
            model: "gpt-5.6-luna".to_owned(),
            prompt: "fixed workload".to_owned(),
            workload_source: WorkloadSource::BuiltIn,
            runs: 8,
            warmups: 1,
            timeout: Duration::from_secs(180),
        };
        let mut cases = benchmark_cases(false);
        let hybrid = cases
            .iter_mut()
            .find(|case| case.path == HYBRID_PATH)
            .ok_or_else(|| io::Error::other("missing Hybrid fixture"))?;
        for sample in &mut hybrid.samples {
            sample.round_e2e = vec![Duration::from_millis(1_500); 3];
        }

        let summary = candidate_summary(&bytes, &settings, &cases)?;

        assert_eq!(summary.candidate_constants.baseline_complete_ms, 2_273);
        assert_eq!(summary.candidate_constants.websocket_complete_saved_ms, 274);
        Ok(())
    }

    #[test]
    fn refuses_candidate_with_fewer_than_eight_retry_free_live_samples() -> Result<(), io::Error> {
        use std::time::Duration;

        use super::super::{BenchmarkSettings, HTTP_PATH, settings::WorkloadSource};
        use super::candidate_summary;

        let bytes = serde_json::to_vec(&profile()?).map_err(io::Error::other)?;
        let settings = BenchmarkSettings {
            upstream: "https://api.ai-cove.com/v1".to_owned(),
            model: "gpt-5.6-luna".to_owned(),
            prompt: "fixed workload".to_owned(),
            workload_source: WorkloadSource::BuiltIn,
            runs: 8,
            warmups: 1,
            timeout: Duration::from_secs(180),
        };
        let mut cases = benchmark_cases(false);
        let http = cases
            .iter_mut()
            .find(|case| case.path == HTTP_PATH)
            .ok_or_else(|| io::Error::other("missing HTTP fixture"))?;
        let sample = http
            .samples
            .first_mut()
            .ok_or_else(|| io::Error::other("missing HTTP sample fixture"))?;
        sample.retries = 1;

        let error = candidate_summary(&bytes, &settings, &cases)
            .expect_err("7 retry-free samples must not produce constants")
            .to_string();

        assert!(error.contains("fewer than 8 retry-free samples"));
        Ok(())
    }

    #[test]
    fn refuses_candidate_with_hybrid_reconnects() -> Result<(), io::Error> {
        use std::time::Duration;

        use super::super::{BenchmarkSettings, HYBRID_PATH, settings::WorkloadSource};
        use super::candidate_summary;

        let bytes = serde_json::to_vec(&profile()?).map_err(io::Error::other)?;
        let settings = BenchmarkSettings {
            upstream: "https://api.ai-cove.com/v1".to_owned(),
            model: "gpt-5.6-luna".to_owned(),
            prompt: "fixed workload".to_owned(),
            workload_source: WorkloadSource::BuiltIn,
            runs: 8,
            warmups: 1,
            timeout: Duration::from_secs(180),
        };
        let mut cases = benchmark_cases(false);
        let hybrid = cases
            .iter_mut()
            .find(|case| case.path == HYBRID_PATH)
            .ok_or_else(|| io::Error::other("missing Hybrid fixture"))?;
        hybrid
            .samples
            .first_mut()
            .ok_or_else(|| io::Error::other("missing Hybrid sample fixture"))?
            .websocket_reconnects = 1;

        let error = candidate_summary(&bytes, &settings, &cases)
            .expect_err("Hybrid reconnects must not produce constants")
            .to_string();

        assert!(error.contains("reconnect"));
        Ok(())
    }

    #[test]
    fn candidate_reports_constant_deltas_and_bandwidth_sensitivity() -> Result<(), io::Error> {
        use std::time::Duration;

        use super::super::{BenchmarkSettings, settings::WorkloadSource};
        use super::candidate_summary;

        let bytes = serde_json::to_vec(&profile()?).map_err(io::Error::other)?;
        let settings = BenchmarkSettings {
            upstream: "https://api.ai-cove.com/v1".to_owned(),
            model: "gpt-5.6-luna".to_owned(),
            prompt: "fixed workload".to_owned(),
            workload_source: WorkloadSource::BuiltIn,
            runs: 8,
            warmups: 1,
            timeout: Duration::from_secs(180),
        };

        let summary = candidate_summary(&bytes, &settings, &benchmark_cases(false))?;
        let at_10_mbps = summary
            .compression_sensitivity
            .iter()
            .find(|item| (item.uplink_mbps - 10.0).abs() < f64::EPSILON)
            .ok_or_else(|| io::Error::other("missing 10 Mbps sensitivity"))?;

        assert_eq!(summary.changes.baseline_first_token_ms.absolute_ms, 0);
        assert!(summary.changes.baseline_first_token_ms.percent.abs() < f64::EPSILON);
        assert!((at_10_mbps.upload_saved_ms - 60.0).abs() < 0.001);
        assert!((at_10_mbps.estimated_first_event_gain_pct - 29.025).abs() < 0.001);
        assert!((at_10_mbps.estimated_complete_gain_pct - 21.75).abs() < 0.001);
        assert!((summary.historical_observation.first_event_gain_pct - 90.0).abs() < 0.001);
        assert!((summary.historical_observation.complete_gain_pct - 50.0).abs() < 0.001);
        Ok(())
    }

    #[test]
    fn candidate_keeps_reproducible_evidence_without_sensitive_fields() -> Result<(), io::Error> {
        use std::time::Duration;

        use super::super::{BenchmarkSettings, settings::WorkloadSource};
        use super::candidate_summary;

        let bytes = serde_json::to_vec(&profile()?).map_err(io::Error::other)?;
        let settings = BenchmarkSettings {
            upstream: "https://api.ai-cove.com/v1".to_owned(),
            model: "gpt-5.6-luna".to_owned(),
            prompt: "must-not-appear-in-summary".to_owned(),
            workload_source: WorkloadSource::BuiltIn,
            runs: 8,
            warmups: 1,
            timeout: Duration::from_secs(180),
        };

        let summary = candidate_summary(&bytes, &settings, &benchmark_cases(false))?;
        let first_day = summary
            .historical_calibration
            .days
            .first()
            .ok_or_else(|| io::Error::other("missing historical day evidence"))?;

        assert_eq!(summary.workload_profile.current_samples, 100);
        assert_eq!(summary.workload_profile.websocket_requests, 90);
        assert_eq!(summary.benchmark.core_paths.len(), 4);
        assert!(
            summary
                .benchmark
                .core_paths
                .iter()
                .all(|path| path.valid_samples == 8)
        );
        assert_eq!(first_day.same_window.sample_count, 1_000);
        assert!((first_day.same_window.coverage_pct - 100.0).abs() < 0.001);
        assert_eq!(
            summary
                .historical_calibration
                .baseline_same_window
                .first_event_p90_ms,
            Some(2_500.0)
        );
        let serialized = serde_json::to_string(&summary).map_err(io::Error::other)?;
        assert!(!serialized.contains("must-not-appear-in-summary"));
        assert!(!serialized.contains("user_id"));
        assert!(!serialized.contains("request_headers"));
        let serialized = serde_json::to_value(&summary).map_err(io::Error::other)?;
        let bucket = serialized
            .pointer("/historicalCalibration/days/0/sameWindow/buckets/0")
            .ok_or_else(|| io::Error::other("missing per-bucket evidence"))?;
        assert_eq!(
            bucket
                .pointer("/bucket/reasoningEffort")
                .and_then(serde_json::Value::as_str),
            Some("high")
        );
        assert_eq!(
            bucket
                .get("currentSamples")
                .and_then(serde_json::Value::as_u64),
            Some(100)
        );
        assert_eq!(
            bucket
                .get("historicalSamples")
                .and_then(serde_json::Value::as_u64),
            Some(1_000)
        );
        assert_eq!(
            bucket
                .get("profileCoveragePct")
                .and_then(serde_json::Value::as_f64),
            Some(100.0)
        );
        Ok(())
    }

    #[test]
    fn renders_human_and_machine_candidate_reports() -> Result<(), io::Error> {
        use std::time::Duration;

        use super::super::{BenchmarkSettings, settings::WorkloadSource};
        use super::{candidate_json, candidate_summary, write_candidate_report};

        let bytes = serde_json::to_vec(&profile()?).map_err(io::Error::other)?;
        let settings = BenchmarkSettings {
            upstream: "https://api.ai-cove.com/v1".to_owned(),
            model: "gpt-5.6-luna".to_owned(),
            prompt: "fixed workload".to_owned(),
            workload_source: WorkloadSource::BuiltIn,
            runs: 8,
            warmups: 1,
            timeout: Duration::from_secs(180),
        };
        let summary = candidate_summary(&bytes, &settings, &benchmark_cases(false))?;
        let mut human = Vec::new();

        write_candidate_report(&mut human, &summary)?;
        let machine = candidate_json(&summary)?;

        let human = String::from_utf8(human).map_err(io::Error::other)?;
        assert!(human.contains("真实使用校准候选（未应用）"));
        assert!(human.contains("旧值 → 候选值"));
        assert!(human.contains("5 Mbps"));
        assert!(human.contains("10 Mbps"));
        assert!(human.contains("20 Mbps"));
        assert!(human.contains("历史 before/after 仅作交叉验证，不归因于 Turbo"));
        assert!(human.contains("逐桶："));
        assert!(human.contains("current_n=100 history_n=1000 coverage=100.0%"));
        let machine: serde_json::Value =
            serde_json::from_slice(&machine).map_err(io::Error::other)?;
        assert_eq!(
            machine
                .pointer("/candidateConstants/baselineFirstTokenMs")
                .and_then(serde_json::Value::as_u64),
            Some(1_661)
        );
        Ok(())
    }

    #[test]
    fn writes_candidate_only_to_the_explicit_output_path() -> Result<(), io::Error> {
        use std::{fs, time::Duration};

        use super::super::{BenchmarkSettings, settings::WorkloadSource};
        use super::generate_candidate_artifacts;

        let directory = tempfile::tempdir()?;
        let profile_path = directory.path().join("profile.json");
        let output_path = directory.path().join("candidate.json");
        fs::write(
            &profile_path,
            serde_json::to_vec(&profile()?).map_err(io::Error::other)?,
        )?;
        let settings = BenchmarkSettings {
            upstream: "https://api.ai-cove.com/v1".to_owned(),
            model: "gpt-5.6-luna".to_owned(),
            prompt: "fixed workload".to_owned(),
            workload_source: WorkloadSource::BuiltIn,
            runs: 8,
            warmups: 1,
            timeout: Duration::from_secs(180),
        };
        let mut human = Vec::new();

        generate_candidate_artifacts(
            &mut human,
            &profile_path,
            &output_path,
            &settings,
            &benchmark_cases(false),
        )?;

        let machine = fs::read(&output_path)?;
        let machine: serde_json::Value =
            serde_json::from_slice(&machine).map_err(io::Error::other)?;
        assert_eq!(
            machine.get("status").and_then(serde_json::Value::as_str),
            Some("candidate_not_applied")
        );
        assert!(
            generate_candidate_artifacts(
                &mut Vec::new(),
                &profile_path,
                &profile_path,
                &settings,
                &benchmark_cases(false),
            )
            .is_err()
        );
        Ok(())
    }
}
