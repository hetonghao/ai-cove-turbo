use std::{
    io::{self, Write},
    time::Duration,
};

use super::{BenchmarkCase, BenchmarkSettings, LatencySummary, Sample, summarize_latency};

#[derive(Clone, Copy, Debug)]
struct CaseReport {
    scenario: &'static str,
    path: &'static str,
    e2e: LatencySummary,
    round_e2e: LatencySummary,
    transport: LatencySummary,
    round_transport: LatencySummary,
    setup: LatencySummary,
    raw_bytes: u64,
    wire_bytes: u64,
    saved_pct: f64,
    logical_requests: u64,
    application_messages: u64,
    response_events: u64,
    websocket_handshakes: u64,
}

fn case_report(case: &BenchmarkCase) -> Result<CaseReport, io::Error> {
    let first = case
        .samples
        .first()
        .ok_or_else(|| io::Error::other("benchmark case has no samples"))?;
    let raw_bytes = first.raw_bytes;
    let wire_bytes = first.wire_bytes;
    let saved = raw_bytes.saturating_sub(wire_bytes);
    let saved_pct = saved
        .saturating_mul(10_000)
        .checked_div(raw_bytes)
        .map_or(0.0, |basis_points| {
            f64::from(u32::try_from(basis_points).unwrap_or(u32::MAX)) / 100.0
        });
    Ok(CaseReport {
        scenario: case.scenario,
        path: case.path,
        e2e: summarize(&case.samples, |sample| sample.e2e)?,
        round_e2e: summarize_rounds(&case.samples, |sample| &sample.round_e2e)?,
        transport: summarize(&case.samples, |sample| sample.transport)?,
        round_transport: summarize_rounds(&case.samples, |sample| &sample.round_transport)?,
        setup: summarize(&case.samples, |sample| sample.setup)?,
        raw_bytes,
        wire_bytes,
        saved_pct,
        logical_requests: first.logical_requests,
        application_messages: first.application_messages,
        response_events: first.response_events,
        websocket_handshakes: first.websocket_handshakes,
    })
}

fn summarize(
    samples: &[Sample],
    value: impl Fn(&Sample) -> Duration,
) -> Result<LatencySummary, io::Error> {
    summarize_latency(&samples.iter().map(value).collect::<Vec<_>>())
        .ok_or_else(|| io::Error::other("benchmark case has no samples"))
}

fn summarize_rounds(
    samples: &[Sample],
    values: impl Fn(&Sample) -> &[Duration],
) -> Result<LatencySummary, io::Error> {
    summarize_latency(
        &samples
            .iter()
            .flat_map(|sample| values(sample).iter().copied())
            .collect::<Vec<_>>(),
    )
    .ok_or_else(|| io::Error::other("benchmark case has no round samples"))
}

fn ratio(baseline_ms: f64, current_ms: f64) -> f64 {
    baseline_ms / current_ms.max(f64::EPSILON)
}

pub(super) fn print_report(
    settings: &BenchmarkSettings,
    cases: &[BenchmarkCase],
) -> Result<(), io::Error> {
    let reports = cases
        .iter()
        .map(case_report)
        .collect::<Result<Vec<_>, _>>()?;
    if reports.is_empty() {
        return Err(io::Error::other("benchmark has no cases"));
    }
    let stdout = io::stdout();
    let mut output = stdout.lock();
    writeln!(
        output,
        "Turbo 3×3 基准（runs={}, warmups={}）",
        settings.runs, settings.warmups
    )?;
    writeln!(
        output,
        "口径：每个场景是一组逻辑请求；WS 每次正式样本只握手一次，再在同一连接发送多条 response.create。"
    )?;
    writeln!(
        output,
        "E2E=完整场景响应；传输=请求体/应用帧发送完成；WS 握手单列；bytes 仅统计应用负载。"
    )?;
    writeln!(
        output,
        "场景 | 技术路径 | 总 E2E median/P95 ms | 每轮 E2E median/P95 ms | 总传输 median/P95 ms | 每轮传输 median/P95 ms | WS 握手 median/P95 ms | 逻辑请求/应用消息/响应事件/握手 | raw → wire | 节省"
    )?;
    writeln!(output, "---|---|---:|---:|---:|---:|---:|---:|---:|---:")?;
    for report in &reports {
        writeln!(
            output,
            "{} | {} | {:.1}/{:.1} | {:.1}/{:.1} | {:.1}/{:.1} | {:.1}/{:.1} | {:.1}/{:.1} | {}/{}/{}/{} | {} → {} | {:.1}%",
            report.scenario,
            report.path,
            report.e2e.median_ms,
            report.e2e.p95_ms,
            report.round_e2e.median_ms,
            report.round_e2e.p95_ms,
            report.transport.median_ms,
            report.transport.p95_ms,
            report.round_transport.median_ms,
            report.round_transport.p95_ms,
            report.setup.median_ms,
            report.setup.p95_ms,
            report.logical_requests,
            report.application_messages,
            report.response_events,
            report.websocket_handshakes,
            report.raw_bytes,
            report.wire_bytes,
            report.saved_pct,
        )?;
        if report.path != "直连（不走 Turbo）" {
            let baseline = reports
                .iter()
                .find(|candidate| {
                    candidate.scenario == report.scenario && candidate.path == "直连（不走 Turbo）"
                })
                .ok_or_else(|| io::Error::other("3×3 benchmark is missing its direct baseline"))?;
            writeln!(
                output,
                "  相对同场景直连：总 E2E {:.2}x，每轮 E2E {:.2}x，传输 {:.2}x",
                ratio(baseline.e2e.median_ms, report.e2e.median_ms),
                ratio(baseline.round_e2e.median_ms, report.round_e2e.median_ms),
                ratio(baseline.transport.median_ms, report.transport.median_ms),
            )?;
        }
    }
    for case in cases {
        writeln!(output, "\n{} / {} samples:", case.scenario, case.path)?;
        for (index, sample) in case.samples.iter().enumerate() {
            writeln!(
                output,
                "  #{} e2e={:.1}ms transport={:.1}ms setup={:.1}ms requests/messages/events/handshakes={}/{}/{}/{} bytes={}->{}",
                index + 1,
                sample.e2e.as_secs_f64() * 1000.0,
                sample.transport.as_secs_f64() * 1000.0,
                sample.setup.as_secs_f64() * 1000.0,
                sample.logical_requests,
                sample.application_messages,
                sample.response_events,
                sample.websocket_handshakes,
                sample.raw_bytes,
                sample.wire_bytes,
            )?;
        }
    }
    Ok(())
}
