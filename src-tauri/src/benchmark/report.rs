use std::io::{self, Write};

use super::{BenchmarkCase, BenchmarkSettings, LatencySummary, summarize_latency};

#[derive(Clone, Copy, Debug)]
struct CaseReport {
    name: &'static str,
    e2e: LatencySummary,
    transport: LatencySummary,
    setup: LatencySummary,
    raw_bytes: u64,
    wire_bytes: u64,
    saved_pct: f64,
}

fn case_report(case: &BenchmarkCase) -> Result<CaseReport, io::Error> {
    let e2e = summarize(&case.samples, |sample| sample.e2e)?;
    let transport = summarize(&case.samples, |sample| sample.transport)?;
    let setup = summarize(&case.samples, |sample| sample.setup)?;
    let raw_bytes = case
        .samples
        .iter()
        .map(|sample| sample.raw_bytes)
        .min()
        .unwrap_or_default();
    let wire_bytes = case
        .samples
        .iter()
        .map(|sample| sample.wire_bytes)
        .min()
        .unwrap_or_default();
    let saved = raw_bytes.saturating_sub(wire_bytes);
    let saved_pct = saved
        .saturating_mul(10_000)
        .checked_div(raw_bytes)
        .map_or(0.0, |basis_points| {
            f64::from(u32::try_from(basis_points).unwrap_or(u32::MAX)) / 100.0
        });
    Ok(CaseReport {
        name: case.name,
        e2e,
        transport,
        setup,
        raw_bytes,
        wire_bytes,
        saved_pct,
    })
}

fn summarize(
    samples: &[super::Sample],
    value: impl Fn(super::Sample) -> std::time::Duration,
) -> Result<LatencySummary, io::Error> {
    summarize_latency(&samples.iter().copied().map(value).collect::<Vec<_>>())
        .ok_or_else(|| io::Error::other("benchmark case has no samples"))
}

pub(super) fn print_report(
    settings: &BenchmarkSettings,
    cases: &[BenchmarkCase],
) -> Result<(), io::Error> {
    let reports = cases
        .iter()
        .map(case_report)
        .collect::<Result<Vec<_>, _>>()?;
    let baseline = reports
        .first()
        .ok_or_else(|| io::Error::other("benchmark has no cases"))?;
    let stdout = io::stdout();
    let mut output = stdout.lock();
    writeln!(
        output,
        "Turbo 三路径基准（runs={}, warmups={}）",
        settings.runs, settings.warmups
    )?;
    writeln!(
        output,
        "口径：E2E=完整响应；传输=请求体交给发送器/WS 应用帧完成；WS 握手单列。"
    )?;
    writeln!(
        output,
        "场景 | E2E median/P95 ms | 传输 median/P95 ms | WS 握手 median/P95 ms | raw → wire | 节省"
    )?;
    writeln!(output, "---|---:|---:|---:|---:|---:")?;
    for report in &reports {
        writeln!(
            output,
            "{} | {:.1}/{:.1} | {:.1}/{:.1} | {:.1}/{:.1} | {} → {} | {:.1}%",
            report.name,
            report.e2e.median_ms,
            report.e2e.p95_ms,
            report.transport.median_ms,
            report.transport.p95_ms,
            report.setup.median_ms,
            report.setup.p95_ms,
            report.raw_bytes,
            report.wire_bytes,
            report.saved_pct,
        )?;
        if report.name != baseline.name {
            writeln!(
                output,
                "  相对 {}：E2E {:.2}x，传输 {:.2}x",
                baseline.name,
                baseline.e2e.median_ms / report.e2e.median_ms,
                baseline.transport.median_ms / report.transport.median_ms,
            )?;
        }
    }
    for case in cases {
        writeln!(output, "\n{} samples:", case.name)?;
        for (index, sample) in case.samples.iter().enumerate() {
            writeln!(
                output,
                "  #{} e2e={:.1}ms transport={:.1}ms setup={:.1}ms bytes={}->{}",
                index + 1,
                sample.e2e.as_secs_f64() * 1000.0,
                sample.transport.as_secs_f64() * 1000.0,
                sample.setup.as_secs_f64() * 1000.0,
                sample.raw_bytes,
                sample.wire_bytes,
            )?;
        }
    }
    Ok(())
}
