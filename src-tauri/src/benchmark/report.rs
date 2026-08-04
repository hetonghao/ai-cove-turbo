use std::{io, time::Duration};

use super::{BenchmarkCase, BenchmarkSettings, LatencyMsSummary, Sample, summarize_latency};

mod render;

pub(super) fn print_report(
    settings: &BenchmarkSettings,
    cases: &[BenchmarkCase],
) -> Result<(), io::Error> {
    render::print_report(settings, cases)
}

#[derive(Clone, Copy, Debug)]
struct CaseReport {
    scenario: &'static str,
    path: &'static str,
    e2e: LatencyMsSummary,
    ttft: Option<LatencyMsSummary>,
    round_e2e: LatencyMsSummary,
    setup: Option<LatencyMsSummary>,
    warm_request: Option<LatencyMsSummary>,
    connection_lifetime: Option<LatencyMsSummary>,
    websocket_reconnects: Option<CountRange>,
    messages_per_connection: Option<CountRange>,
    raw_bytes: u64,
    encoded_bytes: u64,
    reduction_pct: f64,
    logical_requests: u64,
    application_messages: u64,
    response_events: u64,
    websocket_handshakes: u64,
}

#[derive(Clone, Copy, Debug)]
struct CountRange {
    min: u64,
    max: u64,
}

pub(super) fn payload_reduction_pct(raw_bytes: u64, encoded_bytes: u64) -> f64 {
    if raw_bytes == 0 {
        return 0.0;
    }
    let raw = f64::from(u32::try_from(raw_bytes).unwrap_or(u32::MAX));
    let encoded = f64::from(u32::try_from(encoded_bytes).unwrap_or(u32::MAX));
    (raw - encoded) / raw * 100.0
}

pub(super) fn payload_serialization_ms(bytes: u64, megabits_per_second: f64) -> f64 {
    if megabits_per_second <= 0.0 {
        return 0.0;
    }
    let bytes = f64::from(u32::try_from(bytes).unwrap_or(u32::MAX));
    bytes * 8.0 / (megabits_per_second * 1_000_000.0) * 1_000.0
}

fn case_report(case: &BenchmarkCase) -> Result<CaseReport, io::Error> {
    let first = case
        .samples
        .first()
        .ok_or_else(|| io::Error::other("benchmark case has no samples"))?;
    if case.samples.iter().any(|sample| {
        sample.raw_bytes != first.raw_bytes || sample.encoded_bytes != first.encoded_bytes
    }) {
        return Err(io::Error::other(
            "benchmark payload bytes changed between samples",
        ));
    }
    Ok(CaseReport {
        scenario: case.scenario,
        path: case.path,
        e2e: summarize(&case.samples, |sample| sample.e2e)?,
        ttft: Some(summarize_rounds(&case.samples, |sample| {
            &sample.first_events
        })?),
        round_e2e: summarize_rounds(&case.samples, |sample| &sample.round_e2e)?,
        setup: if case
            .samples
            .iter()
            .any(|sample| sample.connection_lifetime.is_some())
        {
            Some(summarize(&case.samples, |sample| sample.setup)?)
        } else {
            None
        },
        warm_request: summarize_optional_rounds(&case.samples, |sample| &sample.warm_round_e2e),
        connection_lifetime: summarize_optional(&case.samples, |sample| sample.connection_lifetime),
        websocket_reconnects: summarize_optional_counts(&case.samples, |sample| {
            sample
                .messages_per_connection
                .map(|_| sample.websocket_reconnects)
        }),
        messages_per_connection: summarize_optional_counts(&case.samples, |sample| {
            sample.messages_per_connection
        }),
        raw_bytes: first.raw_bytes,
        encoded_bytes: first.encoded_bytes,
        reduction_pct: payload_reduction_pct(first.raw_bytes, first.encoded_bytes),
        logical_requests: first.logical_requests,
        application_messages: first.application_messages,
        response_events: first.response_events,
        websocket_handshakes: first.websocket_handshakes,
    })
}

fn summarize(
    samples: &[Sample],
    value: impl Fn(&Sample) -> Duration,
) -> Result<LatencyMsSummary, io::Error> {
    summarize_latency(&samples.iter().map(value).collect::<Vec<_>>())
        .ok_or_else(|| io::Error::other("benchmark case has no samples"))
}

fn summarize_rounds(
    samples: &[Sample],
    values: impl Fn(&Sample) -> &[Duration],
) -> Result<LatencyMsSummary, io::Error> {
    summarize_latency(
        &samples
            .iter()
            .flat_map(|sample| values(sample).iter().copied())
            .collect::<Vec<_>>(),
    )
    .ok_or_else(|| io::Error::other("benchmark case has no round samples"))
}

fn summarize_optional_rounds(
    samples: &[Sample],
    values: impl Fn(&Sample) -> &[Duration],
) -> Option<LatencyMsSummary> {
    summarize_latency(
        &samples
            .iter()
            .flat_map(|sample| values(sample).iter().copied())
            .collect::<Vec<_>>(),
    )
}

fn summarize_optional(
    samples: &[Sample],
    value: impl Fn(&Sample) -> Option<Duration>,
) -> Option<LatencyMsSummary> {
    summarize_latency(&samples.iter().filter_map(value).collect::<Vec<_>>())
}

fn summarize_optional_counts(
    samples: &[Sample],
    value: impl Fn(&Sample) -> Option<u64>,
) -> Option<CountRange> {
    let values = samples.iter().filter_map(value).collect::<Vec<_>>();
    Some(CountRange {
        min: values.iter().copied().min()?,
        max: values.iter().copied().max()?,
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{super::BenchmarkCase, super::Sample, case_report};

    #[test]
    fn summarizes_ttft_warm_request_and_connection_counts() {
        let case = BenchmarkCase {
            scenario: "multi-turn",
            path: "Turbo WS + zstd",
            samples: vec![Sample {
                e2e: Duration::from_millis(80),
                setup: Duration::from_millis(20),
                raw_bytes: 10,
                encoded_bytes: 8,
                logical_requests: 2,
                application_messages: 2,
                response_events: 4,
                websocket_handshakes: 1,
                round_e2e: vec![Duration::from_millis(40), Duration::from_millis(60)],
                first_events: vec![Duration::from_millis(10), Duration::from_millis(30)],
                warm_round_e2e: vec![Duration::from_millis(60)],
                connection_lifetime: Some(Duration::from_millis(70)),
                websocket_reconnects: 0,
                messages_per_connection: Some(2),
            }],
        };

        let report = case_report(&case).expect("complete sample must produce a report");

        assert_eq!(report.ttft.map(|summary| summary.median), Some(20.0));
        assert_eq!(
            report.warm_request.map(|summary| summary.median),
            Some(60.0)
        );
        assert_eq!(
            report.connection_lifetime.map(|summary| summary.median),
            Some(70.0)
        );
        assert_eq!(
            report.websocket_reconnects.map(|summary| summary.min),
            Some(0)
        );
        assert_eq!(
            report.messages_per_connection.map(|summary| summary.max),
            Some(2)
        );
    }
}
