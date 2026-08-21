use super::super::super::{
    BenchmarkCase, BenchmarkResult, CompressionSampleMetrics, RoundTransport, Sample,
};
use super::super::{CaseReport, case_report};
use super::types::{
    CaseMetrics, CompressionMetrics, ConnectionChurnMetrics, CountMetrics, RawMetrics,
    SampleMetrics, SummaryMetrics,
};

pub(super) fn raw_metrics(cases: &[BenchmarkCase]) -> BenchmarkResult<RawMetrics> {
    cases
        .iter()
        .map(|case| {
            let report = case_report(case)?;
            Ok(CaseMetrics {
                scenario: case.scenario.to_owned(),
                path: case.path.to_owned(),
                summary: summary_metrics(&report, case),
                samples: case.samples.iter().map(sample_metrics).collect(),
            })
        })
        .collect::<BenchmarkResult<Vec<_>>>()
        .map(|cases| RawMetrics { cases })
}

fn summary_metrics(report: &CaseReport, case: &BenchmarkCase) -> SummaryMetrics {
    SummaryMetrics {
        e2e_median_ms: metric(report.e2e.median),
        ttft_median_ms: report.ttft.map(|summary| metric(summary.median)),
        raw_bytes: CountMetrics {
            min: report.raw_bytes.min,
            max: report.raw_bytes.max,
        },
        encoded_bytes: CountMetrics {
            min: report.encoded_bytes.min,
            max: report.encoded_bytes.max,
        },
        compression_metrics: aggregate_compression_metrics(&case.samples),
        connection_churn: ConnectionChurnMetrics {
            websocket_handshakes: CountMetrics {
                min: report.websocket_handshakes.min,
                max: report.websocket_handshakes.max,
            },
            websocket_reconnects: report.websocket_reconnects.map(|range| CountMetrics {
                min: range.min,
                max: range.max,
            }),
            messages_per_connection: report.messages_per_connection.map(|range| CountMetrics {
                min: range.min,
                max: range.max,
            }),
        },
        valid_samples: report.valid_samples,
        recovered_samples: report.recovered_samples,
        retries: report.retries,
    }
}

fn sample_metrics(sample: &Sample) -> SampleMetrics {
    SampleMetrics {
        e2e_ms: milliseconds(sample.e2e),
        setup_ms: milliseconds(sample.setup),
        first_events_ms: sample
            .first_events
            .iter()
            .map(|value| milliseconds(*value))
            .collect(),
        round_e2e_ms: sample
            .round_e2e
            .iter()
            .map(|value| milliseconds(*value))
            .collect(),
        warm_round_e2e_ms: sample
            .warm_round_e2e
            .iter()
            .map(|value| milliseconds(*value))
            .collect(),
        connection_lifetime_ms: sample.connection_lifetime.map(milliseconds),
        websocket_reconnects: sample.websocket_reconnects,
        messages_per_connection: sample.messages_per_connection,
        raw_bytes: sample.raw_bytes,
        encoded_bytes: sample.encoded_bytes,
        logical_requests: sample.logical_requests,
        application_messages: sample.application_messages,
        http_requests: sample.http_requests,
        websocket_messages: sample.websocket_messages,
        response_events: sample.response_events,
        websocket_handshakes: sample.websocket_handshakes,
        retries: sample.retries,
        round_transports: sample
            .round_transports
            .iter()
            .map(|transport| match transport {
                RoundTransport::Http => "HTTP".to_owned(),
                RoundTransport::WebSocket => "WS".to_owned(),
            })
            .collect(),
        compression_metrics: sample.compression_metrics.map(compression_metrics),
    }
}

fn compression_metrics(sample: CompressionSampleMetrics) -> CompressionMetrics {
    CompressionMetrics {
        source: "metrics_snapshot_delta".to_owned(),
        encode_count: Some(sample.encode_count),
        decode_count: Some(sample.decode_count),
        queue_wait_ms: Some(count_ms(sample.queue_wait_ms)),
        work_time_ms: Some(count_ms(sample.work_time_ms)),
        failures: Some(sample.failures),
        fast_path_count: Some(sample.fast_path_count),
    }
}

fn count_ms(value: u64) -> f64 {
    f64::from(u32::try_from(value).unwrap_or(u32::MAX))
}

fn aggregate_compression_metrics(samples: &[Sample]) -> CompressionMetrics {
    let Some(metrics) = samples
        .iter()
        .map(|sample| sample.compression_metrics)
        .collect::<Option<Vec<_>>>()
    else {
        return CompressionMetrics {
            source: "not_applicable".to_owned(),
            encode_count: None,
            decode_count: None,
            queue_wait_ms: None,
            work_time_ms: None,
            failures: None,
            fast_path_count: None,
        };
    };
    let total =
        metrics
            .into_iter()
            .fold(CompressionSampleMetrics::default(), |mut total, sample| {
                total.encode_count = total.encode_count.saturating_add(sample.encode_count);
                total.decode_count = total.decode_count.saturating_add(sample.decode_count);
                total.queue_wait_ms = total.queue_wait_ms.saturating_add(sample.queue_wait_ms);
                total.work_time_ms = total.work_time_ms.saturating_add(sample.work_time_ms);
                total.failures = total.failures.saturating_add(sample.failures);
                total.fast_path_count =
                    total.fast_path_count.saturating_add(sample.fast_path_count);
                total
            });
    compression_metrics(total)
}

fn milliseconds(value: std::time::Duration) -> f64 {
    metric(value.as_secs_f64() * 1_000.0)
}

fn metric(value: f64) -> f64 {
    (value * 1_000_000.0).round() / 1_000_000.0
}
