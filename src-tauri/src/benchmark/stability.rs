use std::{io, time::Duration};

use super::BenchmarkCase;

const MIN_CLEAN_SAMPLES: usize = 8;
const MAX_HALF_MEDIAN_DRIFT_PCT: f64 = 15.0;

fn median_ms(values: &[Duration]) -> Option<f64> {
    super::summarize_latency(values).map(|summary| summary.median)
}

fn validate_series(values: &[Duration]) -> io::Result<()> {
    if values.len() < MIN_CLEAN_SAMPLES {
        return Err(io::Error::other(format!(
            "fewer than {MIN_CLEAN_SAMPLES} retry-free samples"
        )));
    }
    let (first, second) = values.split_at(values.len() / 2);
    let first = median_ms(first).ok_or_else(|| io::Error::other("first half has no samples"))?;
    let second = median_ms(second).ok_or_else(|| io::Error::other("second half has no samples"))?;
    if first <= f64::EPSILON {
        return Err(io::Error::other("first-half median must be positive"));
    }
    let drift = (second - first).abs() / first * 100.0;
    if drift > MAX_HALF_MEDIAN_DRIFT_PCT + f64::EPSILON {
        return Err(io::Error::other(format!(
            "half-sample median drift {drift:.1}% exceeds 15%"
        )));
    }
    Ok(())
}

pub(super) fn validate_case(case: &BenchmarkCase) -> io::Result<()> {
    let values = case
        .samples
        .iter()
        .filter(|sample| sample.retries == 0)
        .map(|sample| sample.e2e)
        .collect::<Vec<_>>();
    validate_series(&values).map_err(|error| {
        io::Error::other(format!(
            "benchmark path {} is not candidate-ready: {error}",
            case.path
        ))
    })
}

#[cfg(test)]
mod tests {
    use std::{io, time::Duration};

    use super::validate_series;

    #[test]
    fn accepts_eight_samples_when_half_medians_drift_at_most_fifteen_percent() -> io::Result<()> {
        let values = [100, 100, 100, 100, 115, 115, 115, 115].map(Duration::from_millis);

        validate_series(&values)
    }

    #[test]
    fn rejects_samples_when_half_medians_drift_over_fifteen_percent() {
        let values = [100, 100, 100, 100, 116, 116, 116, 116].map(Duration::from_millis);

        let error = validate_series(&values)
            .expect_err("16% median drift must not produce a candidate")
            .to_string();

        assert!(error.contains("15%"));
    }
}
