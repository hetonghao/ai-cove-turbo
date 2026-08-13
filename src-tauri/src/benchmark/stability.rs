use std::{io, time::Duration};

use super::{BenchmarkCase, Sample};

const MIN_CLEAN_SAMPLES: usize = 8;

#[derive(Clone, Copy)]
pub(super) enum PairedMetric {
    FirstEvent,
    Complete,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct PairedSavings {
    pub(super) median_ms: f64,
    pub(super) drift_pct: Option<f64>,
    pub(super) qualifies: bool,
}

fn median(values: &[f64]) -> Option<f64> {
    let mut values = values.to_vec();
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    let upper = values.get(middle).copied()?;
    if values.len().is_multiple_of(2) {
        Some(values.get(middle.checked_sub(1)?)?.midpoint(upper))
    } else {
        Some(upper)
    }
}

fn metric_values(sample: &Sample, metric: PairedMetric) -> &[Duration] {
    match metric {
        PairedMetric::FirstEvent => &sample.first_events,
        PairedMetric::Complete => &sample.round_e2e,
    }
}

pub(super) fn validate_sample_count(case: &BenchmarkCase) -> io::Result<()> {
    let clean = case
        .samples
        .iter()
        .filter(|sample| sample.retries == 0)
        .count();
    if clean < MIN_CLEAN_SAMPLES {
        return Err(io::Error::other(format!(
            "benchmark path {} is not candidate-ready: fewer than {MIN_CLEAN_SAMPLES} retry-free samples",
            case.path
        )));
    }
    Ok(())
}

pub(super) fn paired_savings(
    http: &BenchmarkCase,
    websocket: &BenchmarkCase,
    metric: PairedMetric,
) -> io::Result<PairedSavings> {
    if http.samples.len() != websocket.samples.len() {
        return Err(io::Error::other(
            "paired benchmark paths have different sample counts",
        ));
    }
    let clean = http
        .samples
        .iter()
        .zip(&websocket.samples)
        .filter(|(http, websocket)| http.retries == 0 && websocket.retries == 0)
        .collect::<Vec<_>>();
    if clean.len() < MIN_CLEAN_SAMPLES {
        return Err(io::Error::other(format!(
            "fewer than {MIN_CLEAN_SAMPLES} paired retry-free samples"
        )));
    }
    let mut savings = Vec::new();
    let mut first_half = Vec::new();
    let mut second_half = Vec::new();
    let split = clean.len() / 2;
    for (index, (http, websocket)) in clean.into_iter().enumerate() {
        let http_values = metric_values(http, metric);
        let websocket_values = metric_values(websocket, metric);
        if http_values.is_empty() || http_values.len() != websocket_values.len() {
            return Err(io::Error::other(
                "paired benchmark samples have different round counts",
            ));
        }
        for (http, websocket) in http_values.iter().zip(websocket_values) {
            let saved_ms = (http.as_secs_f64() - websocket.as_secs_f64()) * 1_000.0;
            savings.push(saved_ms);
            if index < split {
                first_half.push(saved_ms);
            } else {
                second_half.push(saved_ms);
            }
        }
    }
    let median_ms = median(&savings).ok_or_else(|| io::Error::other("paired series is empty"))?;
    let first = median(&first_half).ok_or_else(|| io::Error::other("first half is empty"))?;
    let second = median(&second_half).ok_or_else(|| io::Error::other("second half is empty"))?;
    let drift_pct = (first > f64::EPSILON).then_some((second - first).abs() / first * 100.0);
    Ok(PairedSavings {
        median_ms,
        drift_pct,
        qualifies: median_ms > 0.0 && first > 0.0 && second > 0.0,
    })
}

#[cfg(test)]
mod tests {
    use std::{io, time::Duration};

    use super::super::{BenchmarkCase, RoundTransport, Sample};
    use super::{PairedMetric, paired_savings};

    fn case(path: &'static str, values: [u64; 8]) -> BenchmarkCase {
        BenchmarkCase {
            scenario: "continuation",
            path,
            samples: values
                .into_iter()
                .map(|value| Sample {
                    e2e: Duration::from_millis(value),
                    setup: Duration::ZERO,
                    raw_bytes: 1,
                    encoded_bytes: 1,
                    logical_requests: 1,
                    application_messages: 1,
                    http_requests: 1,
                    websocket_messages: 0,
                    response_events: 1,
                    websocket_handshakes: 0,
                    round_e2e: vec![Duration::from_millis(value)],
                    first_events: vec![Duration::from_millis(value)],
                    warm_round_e2e: Vec::new(),
                    connection_lifetime: None,
                    websocket_reconnects: 0,
                    messages_per_connection: None,
                    retries: 0,
                    round_transports: vec![RoundTransport::Http],
                })
                .collect(),
        }
    }

    #[test]
    fn qualifies_stable_paired_savings() -> io::Result<()> {
        let http = case("HTTP", [600; 8]);
        let websocket = case("WS", [100, 100, 100, 100, 115, 115, 115, 115]);

        let savings = paired_savings(&http, &websocket, PairedMetric::FirstEvent)?;

        assert!(savings.qualifies);
        assert!((savings.median_ms - 492.5).abs() < 0.001);
        Ok(())
    }

    #[test]
    fn rejects_paired_savings_when_a_half_turns_negative() -> io::Result<()> {
        let http = case("HTTP", [600; 8]);
        let websocket = case("WS", [100, 100, 100, 100, 700, 700, 700, 700]);

        let savings = paired_savings(&http, &websocket, PairedMetric::FirstEvent)?;

        assert!(!savings.qualifies);
        Ok(())
    }
}
