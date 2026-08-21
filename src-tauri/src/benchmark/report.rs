use std::{io, time::Duration};

use super::{
    BenchmarkCase, BenchmarkSettings, HYBRID_PATH, LatencyMsSummary, Sample, summarize_latency,
};

mod artifact;
mod render;

pub(super) fn print_report(
    settings: &BenchmarkSettings,
    cases: &[BenchmarkCase],
) -> Result<(), io::Error> {
    render::print_report(settings, cases)
}

pub(super) fn write_metrics_artifact_if_requested(
    settings: &BenchmarkSettings,
    cases: &[BenchmarkCase],
) -> Result<(), io::Error> {
    artifact::write_if_requested(settings, cases)
}

#[derive(Clone, Copy, Debug)]
pub(super) struct CaseReport {
    scenario: &'static str,
    path: &'static str,
    first_turn_e2e: LatencyMsSummary,
    e2e: LatencyMsSummary,
    ttft: Option<LatencyMsSummary>,
    round_e2e: LatencyMsSummary,
    pub(super) http_ttft: Option<LatencyMsSummary>,
    pub(super) websocket_ttft: Option<LatencyMsSummary>,
    pub(super) http_complete: Option<LatencyMsSummary>,
    pub(super) websocket_complete: Option<LatencyMsSummary>,
    setup: Option<LatencyMsSummary>,
    warm_request: Option<LatencyMsSummary>,
    connection_lifetime: Option<LatencyMsSummary>,
    websocket_reconnects: Option<CountRange>,
    messages_per_connection: Option<CountRange>,
    raw_bytes: CountRange,
    encoded_bytes: CountRange,
    reduction_pct: PercentageRange,
    logical_requests: u64,
    http_requests: CountRange,
    websocket_messages: CountRange,
    response_events: CountRange,
    websocket_handshakes: CountRange,
    pub(super) valid_samples: usize,
    recovered_samples: usize,
    retries: u64,
}

#[derive(Clone, Copy, Debug)]
struct CountRange {
    min: u64,
    max: u64,
}

#[derive(Clone, Copy, Debug)]
struct PercentageRange {
    min: f64,
    max: f64,
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

fn validated_samples(case: &BenchmarkCase) -> Result<Vec<&Sample>, io::Error> {
    let first_sample = case
        .samples
        .first()
        .ok_or_else(|| io::Error::other("benchmark case has no samples"))?;
    if case
        .samples
        .iter()
        .any(|sample| sample.logical_requests != first_sample.logical_requests)
    {
        return Err(io::Error::other(format!(
            "benchmark scenario={} path={} logical request count changed between samples",
            case.scenario, case.path
        )));
    }
    if case.path != HYBRID_PATH
        && first_sample.logical_requests == 1
        && case.samples.iter().any(|sample| {
            sample.raw_bytes != first_sample.raw_bytes
                || sample.encoded_bytes != first_sample.encoded_bytes
        })
    {
        return Err(io::Error::other(format!(
            "benchmark scenario={} path={} payload bytes changed between samples",
            case.scenario, case.path
        )));
    }
    let samples = case
        .samples
        .iter()
        .filter(|sample| sample.retries == 0)
        .collect::<Vec<_>>();
    if samples.is_empty() {
        return Err(io::Error::other(format!(
            "benchmark scenario={} path={} 没有无重试的有效样本",
            case.scenario, case.path
        )));
    }
    if samples.iter().any(|sample| {
        sample.round_transports.len() != sample.round_e2e.len()
            || sample.round_transports.len() != sample.first_events.len()
    }) {
        return Err(io::Error::other(format!(
            "benchmark scenario={} path={} 的 RoundTransport 分类不完整",
            case.scenario, case.path
        )));
    }
    Ok(samples)
}

pub(super) fn case_report(case: &BenchmarkCase) -> Result<CaseReport, io::Error> {
    let samples = validated_samples(case)?;
    let first = samples.first().copied().ok_or_else(|| {
        io::Error::other(format!(
            "benchmark scenario={} path={} 没有无重试的有效样本",
            case.scenario, case.path
        ))
    })?;
    let raw_bytes = summarize_counts(&samples, |sample| sample.raw_bytes)?;
    let encoded_bytes = summarize_counts(&samples, |sample| sample.encoded_bytes)?;
    Ok(CaseReport {
        scenario: case.scenario,
        path: case.path,
        first_turn_e2e: summarize(&samples, |sample| {
            sample
                .round_e2e
                .first()
                .copied()
                .map_or(sample.setup, |round| sample.setup + round)
        })?,
        e2e: summarize(&samples, |sample| sample.e2e)?,
        ttft: Some(summarize_rounds(&samples, |sample| &sample.first_events)?),
        round_e2e: summarize_rounds(&samples, |sample| &sample.round_e2e)?,
        http_ttft: summarize_transport_rounds(&samples, super::RoundTransport::Http, |sample| {
            &sample.first_events
        }),
        websocket_ttft: summarize_transport_rounds(
            &samples,
            super::RoundTransport::WebSocket,
            |sample| &sample.first_events,
        ),
        http_complete: summarize_transport_rounds(
            &samples,
            super::RoundTransport::Http,
            |sample| &sample.round_e2e,
        ),
        websocket_complete: summarize_transport_rounds(
            &samples,
            super::RoundTransport::WebSocket,
            |sample| &sample.round_e2e,
        ),
        setup: if samples
            .iter()
            .any(|sample| sample.connection_lifetime.is_some())
        {
            Some(summarize(&samples, |sample| sample.setup)?)
        } else {
            None
        },
        warm_request: summarize_optional_rounds(&samples, |sample| &sample.warm_round_e2e),
        connection_lifetime: summarize_optional(&samples, |sample| sample.connection_lifetime),
        websocket_reconnects: summarize_optional_counts(&samples, |sample| {
            sample
                .messages_per_connection
                .map(|_| sample.websocket_reconnects)
        }),
        messages_per_connection: summarize_optional_counts(&samples, |sample| {
            sample.messages_per_connection
        }),
        raw_bytes,
        encoded_bytes,
        reduction_pct: summarize_reduction(&samples)?,
        logical_requests: first.logical_requests,
        http_requests: summarize_counts(&samples, |sample| sample.http_requests)?,
        websocket_messages: summarize_counts(&samples, |sample| sample.websocket_messages)?,
        response_events: summarize_counts(&samples, |sample| sample.response_events)?,
        websocket_handshakes: summarize_counts(&samples, |sample| sample.websocket_handshakes)?,
        valid_samples: samples.len(),
        recovered_samples: case.samples.len().saturating_sub(samples.len()),
        retries: case.samples.iter().map(|sample| sample.retries).sum(),
    })
}

fn summarize_counts(
    samples: &[&Sample],
    value: impl Fn(&Sample) -> u64,
) -> Result<CountRange, io::Error> {
    summarize_optional_counts(samples, |sample| Some(value(sample)))
        .ok_or_else(|| io::Error::other("benchmark case has no samples"))
}

fn summarize_reduction(samples: &[&Sample]) -> Result<PercentageRange, io::Error> {
    let mut values = samples
        .iter()
        .map(|sample| payload_reduction_pct(sample.raw_bytes, sample.encoded_bytes))
        .collect::<Vec<_>>();
    values.sort_by(f64::total_cmp);
    Ok(PercentageRange {
        min: values
            .first()
            .copied()
            .ok_or_else(|| io::Error::other("benchmark case has no samples"))?,
        max: values
            .last()
            .copied()
            .ok_or_else(|| io::Error::other("benchmark case has no samples"))?,
    })
}

fn summarize(
    samples: &[&Sample],
    value: impl Fn(&Sample) -> Duration,
) -> Result<LatencyMsSummary, io::Error> {
    summarize_latency(
        &samples
            .iter()
            .map(|sample| value(sample))
            .collect::<Vec<_>>(),
    )
    .ok_or_else(|| io::Error::other("benchmark case has no samples"))
}

fn summarize_rounds(
    samples: &[&Sample],
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
    samples: &[&Sample],
    values: impl Fn(&Sample) -> &[Duration],
) -> Option<LatencyMsSummary> {
    summarize_latency(
        &samples
            .iter()
            .flat_map(|sample| values(sample).iter().copied())
            .collect::<Vec<_>>(),
    )
}

fn summarize_transport_rounds(
    samples: &[&Sample],
    transport: super::RoundTransport,
    values: impl Fn(&Sample) -> &[Duration],
) -> Option<LatencyMsSummary> {
    summarize_latency(
        &samples
            .iter()
            .flat_map(|sample| {
                sample
                    .round_transports
                    .iter()
                    .zip(values(sample))
                    .filter_map(|(actual, value)| (*actual == transport).then_some(*value))
            })
            .collect::<Vec<_>>(),
    )
}

fn summarize_optional(
    samples: &[&Sample],
    value: impl Fn(&Sample) -> Option<Duration>,
) -> Option<LatencyMsSummary> {
    summarize_latency(
        &samples
            .iter()
            .filter_map(|sample| value(sample))
            .collect::<Vec<_>>(),
    )
}

fn summarize_optional_counts(
    samples: &[&Sample],
    value: impl Fn(&Sample) -> Option<u64>,
) -> Option<CountRange> {
    let values = samples
        .iter()
        .filter_map(|sample| value(sample))
        .collect::<Vec<_>>();
    Some(CountRange {
        min: values.iter().copied().min()?,
        max: values.iter().copied().max()?,
    })
}

#[cfg(test)]
mod tests {
    use std::{io, time::Duration};

    use super::{super::BenchmarkCase, super::BenchmarkResult, super::Sample, case_report};

    #[test]
    fn summarizes_ttft_warm_request_and_connection_counts() -> BenchmarkResult<()> {
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
                http_requests: 0,
                websocket_messages: 2,
                response_events: 4,
                websocket_handshakes: 1,
                round_e2e: vec![Duration::from_millis(40), Duration::from_millis(60)],
                first_events: vec![Duration::from_millis(10), Duration::from_millis(30)],
                warm_round_e2e: vec![Duration::from_millis(60)],
                connection_lifetime: Some(Duration::from_millis(70)),
                websocket_reconnects: 0,
                messages_per_connection: Some(2),
                retries: 0,
                round_transports: vec![
                    super::super::RoundTransport::WebSocket,
                    super::super::RoundTransport::WebSocket,
                ],
                compression_metrics: None,
            }],
        };

        let report = case_report(&case).map_err(|error| io::Error::other(error.to_string()))?;

        assert!(
            (report.first_turn_e2e.median - 60.0).abs() < f64::EPSILON,
            "first-turn E2E includes WS setup plus the first round"
        );
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
        Ok(())
    }

    #[test]
    fn splits_hybrid_round_latency_by_actual_transport() -> BenchmarkResult<()> {
        let case = BenchmarkCase {
            scenario: "multi-turn",
            path: super::super::HYBRID_PATH,
            samples: vec![Sample {
                e2e: Duration::from_millis(180),
                setup: Duration::from_millis(20),
                raw_bytes: 30,
                encoded_bytes: 20,
                logical_requests: 3,
                application_messages: 3,
                http_requests: 1,
                websocket_messages: 2,
                response_events: 3,
                websocket_handshakes: 1,
                round_e2e: vec![
                    Duration::from_millis(100),
                    Duration::from_millis(20),
                    Duration::from_millis(40),
                ],
                first_events: vec![
                    Duration::from_millis(50),
                    Duration::from_millis(5),
                    Duration::from_millis(15),
                ],
                warm_round_e2e: vec![Duration::from_millis(20), Duration::from_millis(40)],
                connection_lifetime: Some(Duration::from_millis(160)),
                websocket_reconnects: 0,
                messages_per_connection: Some(2),
                retries: 0,
                round_transports: vec![
                    super::super::RoundTransport::Http,
                    super::super::RoundTransport::WebSocket,
                    super::super::RoundTransport::WebSocket,
                ],
                compression_metrics: None,
            }],
        };

        let report = case_report(&case)?;

        assert_eq!(report.http_ttft.map(|summary| summary.median), Some(50.0));
        assert_eq!(
            report.websocket_ttft.map(|summary| summary.median),
            Some(10.0)
        );
        assert_eq!(
            report.http_complete.map(|summary| summary.median),
            Some(100.0)
        );
        assert_eq!(
            report.websocket_complete.map(|summary| summary.median),
            Some(30.0)
        );
        Ok(())
    }

    #[test]
    fn accepts_hybrid_bytes_that_follow_transport_mix() -> BenchmarkResult<()> {
        let sample = |raw_bytes,
                      encoded_bytes,
                      http_requests,
                      websocket_messages,
                      response_events,
                      websocket_handshakes,
                      retries,
                      round_transports| Sample {
            e2e: Duration::from_millis(80),
            setup: Duration::from_millis(20),
            raw_bytes,
            encoded_bytes,
            logical_requests: 5,
            application_messages: 5,
            http_requests,
            websocket_messages,
            response_events,
            websocket_handshakes,
            round_e2e: vec![Duration::from_millis(40); 5],
            first_events: vec![Duration::from_millis(10); 5],
            warm_round_e2e: vec![],
            connection_lifetime: Some(Duration::from_millis(70)),
            websocket_reconnects: 0,
            messages_per_connection: Some(websocket_messages),
            retries,
            round_transports,
            compression_metrics: None,
        };
        let case = BenchmarkCase {
            scenario: "multi-turn",
            path: "local-WS Hybrid",
            samples: vec![
                sample(
                    10,
                    8,
                    2,
                    3,
                    6,
                    1,
                    0,
                    vec![
                        super::super::RoundTransport::Http,
                        super::super::RoundTransport::Http,
                        super::super::RoundTransport::WebSocket,
                        super::super::RoundTransport::WebSocket,
                        super::super::RoundTransport::WebSocket,
                    ],
                ),
                sample(
                    11,
                    7,
                    1,
                    4,
                    5,
                    0,
                    0,
                    vec![
                        super::super::RoundTransport::Http,
                        super::super::RoundTransport::WebSocket,
                        super::super::RoundTransport::WebSocket,
                        super::super::RoundTransport::WebSocket,
                        super::super::RoundTransport::WebSocket,
                    ],
                ),
            ],
        };

        let report = case_report(&case).map_err(|error| io::Error::other(error.to_string()))?;
        assert_eq!(report.raw_bytes.min, 10);
        assert_eq!(report.raw_bytes.max, 11);
        assert_eq!(report.encoded_bytes.min, 7);
        assert_eq!(report.encoded_bytes.max, 8);
        assert!((report.reduction_pct.min - 20.0).abs() < f64::EPSILON);
        assert!((report.reduction_pct.max - (400.0 / 11.0)).abs() < 0.001);
        assert_eq!(report.http_requests.min, 1);
        assert_eq!(report.http_requests.max, 2);
        assert_eq!(report.websocket_messages.min, 3);
        assert_eq!(report.websocket_messages.max, 4);
        assert_eq!(report.response_events.min, 5);
        assert_eq!(report.response_events.max, 6);
        assert_eq!(report.websocket_handshakes.min, 0);
        assert_eq!(report.websocket_handshakes.max, 1);
        assert_eq!(report.retries, 0);
        Ok(())
    }

    #[test]
    fn rejects_non_hybrid_payload_byte_drift_with_context() -> BenchmarkResult<()> {
        let sample = |raw_bytes, encoded_bytes| Sample {
            e2e: Duration::from_millis(80),
            setup: Duration::from_millis(20),
            raw_bytes,
            encoded_bytes,
            logical_requests: 1,
            application_messages: 1,
            http_requests: 0,
            websocket_messages: 1,
            response_events: 1,
            websocket_handshakes: 1,
            round_e2e: vec![Duration::from_millis(40)],
            first_events: vec![Duration::from_millis(10)],
            warm_round_e2e: vec![],
            connection_lifetime: Some(Duration::from_millis(70)),
            websocket_reconnects: 0,
            messages_per_connection: Some(1),
            retries: 0,
            round_transports: vec![super::super::RoundTransport::WebSocket],
            compression_metrics: None,
        };
        let case = BenchmarkCase {
            scenario: "single-turn",
            path: "Turbo WS + zstd",
            samples: vec![sample(10, 8), sample(11, 7)],
        };

        let error = match case_report(&case) {
            Ok(_) => {
                return Err(io::Error::other(
                    "deterministic path payload drift did not fail",
                ));
            }
            Err(error) => error.to_string(),
        };
        assert!(error.contains("benchmark scenario=single-turn path=Turbo WS + zstd"));
        assert!(error.contains("payload bytes changed between samples"));
        Ok(())
    }

    #[test]
    fn accepts_continuation_byte_drift_from_response_ids() -> BenchmarkResult<()> {
        let sample = |raw_bytes| Sample {
            e2e: Duration::from_millis(80),
            setup: Duration::ZERO,
            raw_bytes,
            encoded_bytes: raw_bytes,
            logical_requests: 2,
            application_messages: 2,
            http_requests: 2,
            websocket_messages: 0,
            response_events: 2,
            websocket_handshakes: 0,
            round_e2e: vec![Duration::from_millis(40); 2],
            first_events: vec![Duration::from_millis(10); 2],
            warm_round_e2e: vec![Duration::from_millis(40)],
            connection_lifetime: None,
            websocket_reconnects: 0,
            messages_per_connection: None,
            retries: 0,
            round_transports: vec![super::super::RoundTransport::Http; 2],
            compression_metrics: None,
        };
        let case = BenchmarkCase {
            scenario: "continuation",
            path: super::super::HTTP_PATH,
            samples: vec![sample(100), sample(105)],
        };

        let report = case_report(&case)?;

        assert_eq!(report.raw_bytes.min, 100);
        assert_eq!(report.raw_bytes.max, 105);
        Ok(())
    }

    #[test]
    fn excludes_recovered_sample_from_latency_distribution() -> BenchmarkResult<()> {
        let sample = |e2e_ms, retries| Sample {
            e2e: Duration::from_millis(e2e_ms),
            setup: Duration::from_millis(20),
            raw_bytes: 10,
            encoded_bytes: 8,
            logical_requests: 1,
            application_messages: 1,
            http_requests: 1,
            websocket_messages: 0,
            response_events: 1,
            websocket_handshakes: 0,
            round_e2e: vec![Duration::from_millis(e2e_ms)],
            first_events: vec![Duration::from_millis(e2e_ms / 2)],
            warm_round_e2e: vec![],
            connection_lifetime: None,
            websocket_reconnects: 0,
            messages_per_connection: None,
            retries,
            round_transports: vec![super::super::RoundTransport::Http],
            compression_metrics: None,
        };
        let case = BenchmarkCase {
            scenario: "single-turn",
            path: "Direct HTTPS",
            samples: vec![sample(800, 1), sample(80, 0)],
        };

        let report = case_report(&case).map_err(|error| io::Error::other(error.to_string()))?;

        assert!((report.e2e.median - 80.0).abs() < f64::EPSILON);
        assert_eq!(report.valid_samples, 1);
        assert_eq!(report.recovered_samples, 1);
        assert_eq!(report.retries, 1);
        Ok(())
    }

    #[test]
    fn rejects_case_without_retry_free_samples() {
        let case = BenchmarkCase {
            scenario: "single-turn",
            path: "Direct HTTPS",
            samples: vec![Sample {
                e2e: Duration::from_millis(80),
                setup: Duration::ZERO,
                raw_bytes: 10,
                encoded_bytes: 10,
                logical_requests: 1,
                application_messages: 1,
                http_requests: 1,
                websocket_messages: 0,
                response_events: 1,
                websocket_handshakes: 0,
                round_e2e: vec![Duration::from_millis(80)],
                first_events: vec![Duration::from_millis(40)],
                warm_round_e2e: vec![],
                connection_lifetime: None,
                websocket_reconnects: 0,
                messages_per_connection: None,
                retries: 1,
                round_transports: vec![super::super::RoundTransport::Http],
                compression_metrics: None,
            }],
        };

        let error = case_report(&case)
            .expect_err("all-recovered case must not produce a measurement distribution")
            .to_string();

        assert!(error.contains("scenario=single-turn path=Direct HTTPS"));
        assert!(error.contains("没有无重试的有效样本"));
    }
}
