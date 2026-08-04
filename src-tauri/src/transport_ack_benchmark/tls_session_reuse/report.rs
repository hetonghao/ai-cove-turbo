use std::{fmt::Write as _, io};

use super::{SAMPLE_COUNT, SessionSample};

fn summarize(values: &[f64]) -> Result<String, io::Error> {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let min = *sorted
        .first()
        .ok_or_else(|| io::Error::other("cannot summarize empty ACK samples"))?;
    let max = *sorted
        .last()
        .ok_or_else(|| io::Error::other("cannot summarize empty ACK samples"))?;
    let lower = *sorted
        .get((sorted.len() - 1) / 2)
        .ok_or_else(|| io::Error::other("cannot summarize empty ACK samples"))?;
    let upper = *sorted
        .get(sorted.len() / 2)
        .ok_or_else(|| io::Error::other("cannot summarize empty ACK samples"))?;
    Ok(format!(
        "{:.3}[{:.3},{:.3}]",
        lower.midpoint(upper),
        min,
        max
    ))
}

fn render_summary(phase: &str, all_samples: &[SessionSample]) -> Result<String, io::Error> {
    let samples = all_samples
        .iter()
        .filter(|sample| phase == "all" || sample.phase == phase)
        .collect::<Vec<_>>();
    let setup = summarize(
        &samples
            .iter()
            .map(|sample| sample.setup_ms)
            .collect::<Vec<_>>(),
    )?;
    let ack_rtt = summarize(
        &samples
            .iter()
            .map(|sample| sample.ack_rtt_ms)
            .collect::<Vec<_>>(),
    )?;
    Ok(format!(
        "issue=28 summary phase={phase} count={} setup_ms={setup} ack_rtt_ms={ack_rtt}",
        samples.len()
    ))
}

fn render_report(samples: &[SessionSample]) -> Result<String, io::Error> {
    let mut report = format!(
        "issue=28 upstream=production_ack proxy_state=same_instance samples={SAMPLE_COUNT}\n"
    );
    for sample in samples {
        let _ = writeln!(
            report,
            "issue=28 sample={} phase={} setup_ms={:.3} ack_rtt_ms={:.3} receive_ms={:.3} decode_ms={:.3} wire_bytes={} decoded_bytes={}",
            sample.index,
            sample.phase,
            sample.setup_ms,
            sample.ack_rtt_ms,
            sample.ack.receive_ms,
            sample.ack.decode_ms,
            sample.ack.wire_bytes,
            sample.ack.decoded_bytes,
        );
    }
    for phase in ["cold", "warm", "all"] {
        report.push_str(&render_summary(phase, samples)?);
        report.push('\n');
    }
    Ok(report)
}

pub(super) fn print_report(samples: &[SessionSample]) -> Result<(), io::Error> {
    print!("{}", render_report(samples)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io;

    fn sample(index: usize, phase: &'static str) -> super::super::SessionSample {
        super::super::SessionSample {
            index,
            phase,
            setup_ms: 1.0,
            ack_rtt_ms: 2.0,
            ack: super::super::super::TransportAck {
                ok: true,
                transport: "websocket".to_owned(),
                wire_bytes: 4,
                decoded_bytes: 64,
                receive_ms: 0.5,
                decode_ms: 0.25,
            },
        }
    }

    #[test]
    fn summarizes_values_as_median_min_max() {
        assert_eq!(
            super::summarize(&[3.0, 1.0, 2.0]).ok().as_deref(),
            Some("2.000[1.000,3.000]")
        );
        assert_eq!(
            super::summarize(&[4.0, 1.0, 3.0, 2.0]).ok().as_deref(),
            Some("2.500[1.000,4.000]")
        );
    }

    #[test]
    fn renders_all_samples_after_warm_summary() {
        let rendered = super::render_report(&[sample(1, "cold"), sample(2, "warm")]);
        assert!(rendered.is_ok());
        let Some(rendered) = rendered.ok() else {
            return;
        };
        assert!(rendered.contains("issue=28 sample=1 phase=cold"));
        assert!(rendered.contains("wire_bytes=4 decoded_bytes=64"));
        let Some(warm_index) = rendered.find("issue=28 summary phase=warm count=1") else {
            return;
        };
        let Some(all_index) = rendered.find("issue=28 summary phase=all count=2") else {
            return;
        };
        assert!(warm_index < all_index);
    }

    #[test]
    fn empty_samples_return_io_error() {
        let result = super::render_report(&[]);
        assert!(result.is_err());
        let Err(error) = result else {
            return;
        };
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert_eq!(error.to_string(), "cannot summarize empty ACK samples");
    }
}
