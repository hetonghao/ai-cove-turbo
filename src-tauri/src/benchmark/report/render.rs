use std::{
    io::{self, Write},
    time::Duration,
};

use super::super::{BenchmarkSettings, DIRECT_PATH, summarize_latency, workload_fingerprint};
use super::{
    BenchmarkCase, CountRange, LatencyMsSummary, Sample, case_report, payload_serialization_ms,
};

const REFERENCE_UPLINK_MBPS: f64 = 10.0;

fn observed_e2e_delta_pct(baseline_ms: f64, current_ms: f64) -> f64 {
    if baseline_ms <= f64::EPSILON {
        return 0.0;
    }
    (current_ms - baseline_ms) / baseline_ms * 100.0
}

fn write_latency(output: &mut impl Write, summary: LatencyMsSummary) -> io::Result<()> {
    write!(
        output,
        "{:.1}[{:.1},{:.1}]",
        summary.median, summary.min, summary.max
    )
}

fn write_optional_latency(
    output: &mut impl Write,
    summary: Option<LatencyMsSummary>,
) -> io::Result<()> {
    match summary {
        Some(summary) => write_latency(output, summary),
        None => write!(output, "—"),
    }
}

fn write_optional_count(output: &mut impl Write, summary: Option<CountRange>) -> io::Result<()> {
    match summary {
        Some(summary) if summary.min == summary.max => write!(output, "{}", summary.min),
        Some(summary) => write!(output, "{}–{}", summary.min, summary.max),
        None => write!(output, "—"),
    }
}

fn write_optional_duration(output: &mut impl Write, value: Option<Duration>) -> io::Result<()> {
    match value {
        Some(value) => write!(output, "{:.1}", value.as_secs_f64() * 1000.0),
        None => write!(output, "—"),
    }
}

fn write_sample_report(output: &mut impl Write, index: usize, sample: &Sample) -> io::Result<()> {
    write!(
        output,
        "  #{} e2e={:.1}ms ttft=",
        index + 1,
        sample.e2e.as_secs_f64() * 1000.0,
    )?;
    write_optional_latency(output, summarize_latency(&sample.first_events))?;
    write!(output, "ms complete=")?;
    write_optional_latency(output, summarize_latency(&sample.round_e2e))?;
    write!(output, "ms cold_setup=")?;
    write_optional_duration(output, sample.connection_lifetime.map(|_| sample.setup))?;
    write!(output, "ms warm_request=")?;
    write_optional_latency(output, summarize_latency(&sample.warm_round_e2e))?;
    write!(output, "ms lifetime=")?;
    write_optional_duration(output, sample.connection_lifetime)?;
    write!(output, "ms reconnects=")?;
    write_optional_count(
        output,
        sample.messages_per_connection.map(|_| CountRange {
            min: sample.websocket_reconnects,
            max: sample.websocket_reconnects,
        }),
    )?;
    write!(output, " messages/connection=")?;
    write_optional_count(
        output,
        sample.messages_per_connection.map(|messages| CountRange {
            min: messages,
            max: messages,
        }),
    )?;
    writeln!(
        output,
        " requests/messages/events/handshakes={}/{}/{}/{} bytes={}->{}",
        sample.logical_requests,
        sample.application_messages,
        sample.response_events,
        sample.websocket_handshakes,
        sample.raw_bytes,
        sample.encoded_bytes,
    )
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
        "Turbo 3×3 基准（runs={}, warmups={}，负载来源={}，FNV-1a={:016x}，长输入={} bytes）",
        settings.runs,
        settings.warmups,
        settings.workload_source.label(),
        workload_fingerprint(settings.prompt.as_bytes()),
        settings.prompt.len()
    )?;
    writeln!(
        output,
        "上游={}，模型={}",
        settings.upstream, settings.model
    )?;
    writeln!(
        output,
        "口径：每个样本公网连接独立、样本内多轮复用；路径按轮次交错执行；TTFT=首个有效 SSE/首个 WS 应用数据；complete=完整响应；不输出伪公网传输时间。"
    )?;
    writeln!(
        output,
        "字节=请求应用负载；10 Mbps 仅为 payload 序列化理论值，不含 RTT、协议头、重传、响应和推理。"
    )?;
    writeln!(
        output,
        "场景 | 技术路径 | 总 E2E complete median[min,max] ms | TTFT median[min,max] ms | 每轮 complete median[min,max] ms | WS cold setup median[min,max] ms | warm request median[min,max] ms | connection lifetime median[min,max] ms | reconnects | messages/connection | 请求/消息/响应事件/握手 | 原始正文 → 编码负载 | 减少率 | payload@10Mbps ms"
    )?;
    writeln!(
        output,
        "---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:"
    )?;
    for report in &reports {
        write!(output, "{} | {} | ", report.scenario, report.path)?;
        write_latency(&mut output, report.e2e)?;
        write!(output, " | ")?;
        write_optional_latency(&mut output, report.ttft)?;
        write!(output, " | ")?;
        write_latency(&mut output, report.round_e2e)?;
        write!(output, " | ")?;
        write_optional_latency(&mut output, report.setup)?;
        write!(output, " | ")?;
        write_optional_latency(&mut output, report.warm_request)?;
        write!(output, " | ")?;
        write_optional_latency(&mut output, report.connection_lifetime)?;
        write!(output, " | ")?;
        write_optional_count(&mut output, report.websocket_reconnects)?;
        write!(output, " | ")?;
        write_optional_count(&mut output, report.messages_per_connection)?;
        writeln!(
            output,
            " | {}/{}/{}/{} | {} → {} | {:.1}% | {:.2} → {:.2}",
            report.logical_requests,
            report.application_messages,
            report.response_events,
            report.websocket_handshakes,
            report.raw_bytes,
            report.encoded_bytes,
            report.reduction_pct,
            payload_serialization_ms(report.raw_bytes, REFERENCE_UPLINK_MBPS),
            payload_serialization_ms(report.encoded_bytes, REFERENCE_UPLINK_MBPS),
        )?;
        if report.path != DIRECT_PATH {
            let baseline = reports
                .iter()
                .find(|candidate| {
                    candidate.scenario == report.scenario && candidate.path == DIRECT_PATH
                })
                .ok_or_else(|| io::Error::other("3×3 benchmark is missing its direct baseline"))?;
            writeln!(
                output,
                "  相对同场景直连的观测 E2E 中位数差：{:+.1}%（仅观测，不归因于 Turbo）",
                observed_e2e_delta_pct(baseline.e2e.median, report.e2e.median),
            )?;
        }
    }
    for case in cases {
        writeln!(output, "\n{} / {} samples:", case.scenario, case.path)?;
        for (index, sample) in case.samples.iter().enumerate() {
            write_sample_report(&mut output, index, sample)?;
        }
    }
    Ok(())
}
