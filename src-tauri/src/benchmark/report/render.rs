use std::{
    io::{self, Write},
    time::Duration,
};

use super::super::{
    BenchmarkSettings, DIRECT_PATH, RoundTransport, summarize_latency, workload_fingerprint,
};
use super::{
    BenchmarkCase, CountRange, LatencyMsSummary, PercentageRange, Sample, case_report,
    payload_serialization_ms, summarize_transport_rounds,
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
        Some(summary) => write_count_range(output, summary),
        None => write!(output, "—"),
    }
}

fn write_count_range(output: &mut impl Write, summary: CountRange) -> io::Result<()> {
    if summary.min == summary.max {
        write!(output, "{}", summary.min)
    } else {
        write!(output, "{}–{}", summary.min, summary.max)
    }
}

fn write_percentage_range(output: &mut impl Write, summary: PercentageRange) -> io::Result<()> {
    if (summary.min - summary.max).abs() < f64::EPSILON {
        write!(output, "{:.1}", summary.min)
    } else {
        write!(output, "{:.1}–{:.1}", summary.min, summary.max)
    }
}

fn write_payload_time_range(
    output: &mut impl Write,
    bytes: CountRange,
    megabits_per_second: f64,
) -> io::Result<()> {
    let min = payload_serialization_ms(bytes.min, megabits_per_second);
    let max = payload_serialization_ms(bytes.max, megabits_per_second);
    if bytes.min == bytes.max {
        write!(output, "{min:.2}")
    } else {
        write!(output, "{min:.2}–{max:.2}")
    }
}

fn write_optional_duration(output: &mut impl Write, value: Option<Duration>) -> io::Result<()> {
    match value {
        Some(value) => write!(output, "{:.1}", value.as_secs_f64() * 1000.0),
        None => write!(output, "—"),
    }
}

fn write_sample_report(output: &mut impl Write, index: usize, sample: &Sample) -> io::Result<()> {
    let first_turn_e2e = sample
        .round_e2e
        .first()
        .copied()
        .map_or(sample.setup, |round| sample.setup + round);
    write!(
        output,
        "  #{} first_turn={:.1}ms e2e={:.1}ms ttft=",
        index + 1,
        first_turn_e2e.as_secs_f64() * 1000.0,
        sample.e2e.as_secs_f64() * 1000.0,
    )?;
    write_optional_latency(output, summarize_latency(&sample.first_events))?;
    write!(output, "ms complete=")?;
    write_optional_latency(output, summarize_latency(&sample.round_e2e))?;
    let samples = [sample];
    write!(output, "ms http_ttft=")?;
    write_optional_latency(
        output,
        summarize_transport_rounds(&samples, RoundTransport::Http, |sample| {
            &sample.first_events
        }),
    )?;
    write!(output, "ms ws_ttft=")?;
    write_optional_latency(
        output,
        summarize_transport_rounds(&samples, RoundTransport::WebSocket, |sample| {
            &sample.first_events
        }),
    )?;
    write!(output, "ms http_complete=")?;
    write_optional_latency(
        output,
        summarize_transport_rounds(&samples, RoundTransport::Http, |sample| &sample.round_e2e),
    )?;
    write!(output, "ms ws_complete=")?;
    write_optional_latency(
        output,
        summarize_transport_rounds(&samples, RoundTransport::WebSocket, |sample| {
            &sample.round_e2e
        }),
    )?;
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
    write!(output, " retries={} transports=", sample.retries)?;
    for (index, transport) in sample.round_transports.iter().enumerate() {
        if index > 0 {
            write!(output, "→")?;
        }
        write!(output, "{}", transport.label())?;
    }
    writeln!(
        output,
        " logical_requests={} http_requests={} websocket_messages={} events={} handshakes={} bytes={}->{}",
        sample.logical_requests,
        sample.http_requests,
        sample.websocket_messages,
        sample.response_events,
        sample.websocket_handshakes,
        sample.raw_bytes,
        sample.encoded_bytes,
    )
}

fn write_report_header(output: &mut impl Write, settings: &BenchmarkSettings) -> io::Result<()> {
    writeln!(
        output,
        "Turbo 4 路模型基准（runs={}, warmups={}，负载来源={}，FNV-1a={:016x}，长输入={} bytes）",
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
        "证据分层：本表只报告模型体验；Transport ACK RTT、服务端 receive/decode 与压缩 CPU 仅来自独立套件，不与模型 E2E 混合。"
    )?;
    writeln!(
        output,
        "样本有效性：主表只汇总无重试样本；重试后恢复成功的样本仅计入恢复/重试统计，仍保留在原始明细。"
    )?;
    writeln!(
        output,
        "Hybrid 生命周期：冷启动 HTTP 单独取证；正式样本等待池 ready 后全部使用 WS。"
    )?;
    writeln!(
        output,
        "真实校准：只有同时提供匿名 workload profile 与显式候选输出路径时才生成候选常量；不会自动修改产品代码。"
    )?;
    writeln!(
        output,
        "场景 | 技术路径 | 首轮 E2E median[min,max] ms | 完整会话 E2E median[min,max] ms | TTFT（混合辅助）median[min,max] ms | 每轮 complete（混合辅助）median[min,max] ms | HTTP TTFT median[min,max] ms | WS TTFT median[min,max] ms | HTTP complete median[min,max] ms | WS complete median[min,max] ms | WS setup median[min,max] ms | warm request median[min,max] ms | connection lifetime median[min,max] ms | reconnects | messages/connection | 有效/恢复/重试 | 逻辑请求/HTTP请求/WS消息/响应事件/握手 | 原始正文 → 编码负载 | 减少率 | payload@10Mbps ms"
    )?;
    writeln!(
        output,
        "---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:"
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
    write_report_header(&mut output, settings)?;
    for report in &reports {
        write!(output, "{} | {} | ", report.scenario, report.path)?;
        write_latency(&mut output, report.first_turn_e2e)?;
        write!(output, " | ")?;
        write_latency(&mut output, report.e2e)?;
        write!(output, " | ")?;
        write_optional_latency(&mut output, report.ttft)?;
        write!(output, " | ")?;
        write_latency(&mut output, report.round_e2e)?;
        write!(output, " | ")?;
        write_optional_latency(&mut output, report.http_ttft)?;
        write!(output, " | ")?;
        write_optional_latency(&mut output, report.websocket_ttft)?;
        write!(output, " | ")?;
        write_optional_latency(&mut output, report.http_complete)?;
        write!(output, " | ")?;
        write_optional_latency(&mut output, report.websocket_complete)?;
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
        write!(output, " | ")?;
        write!(
            output,
            "{}/{}/{} | ",
            report.valid_samples, report.recovered_samples, report.retries
        )?;
        write_count_range(
            &mut output,
            CountRange {
                min: report.logical_requests,
                max: report.logical_requests,
            },
        )?;
        write!(output, "/")?;
        write_count_range(&mut output, report.http_requests)?;
        write!(output, "/")?;
        write_count_range(&mut output, report.websocket_messages)?;
        write!(output, "/")?;
        write_count_range(&mut output, report.response_events)?;
        write!(output, "/")?;
        write_count_range(&mut output, report.websocket_handshakes)?;
        write!(output, " | ")?;
        write_count_range(&mut output, report.raw_bytes)?;
        write!(output, " → ")?;
        write_count_range(&mut output, report.encoded_bytes)?;
        write!(output, " | ")?;
        write_percentage_range(&mut output, report.reduction_pct)?;
        write!(output, "% | ")?;
        write_payload_time_range(&mut output, report.raw_bytes, REFERENCE_UPLINK_MBPS)?;
        write!(output, " → ")?;
        write_payload_time_range(&mut output, report.encoded_bytes, REFERENCE_UPLINK_MBPS)?;
        writeln!(output)?;
        if report.path != DIRECT_PATH {
            let baseline = reports
                .iter()
                .find(|candidate| {
                    candidate.scenario == report.scenario && candidate.path == DIRECT_PATH
                })
                .ok_or_else(|| {
                    io::Error::other("four-path benchmark is missing its direct baseline")
                })?;
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

#[cfg(test)]
mod tests {
    use std::{io, time::Duration};

    use super::super::super::settings::WorkloadSource;
    use super::super::super::{BenchmarkSettings, Sample};
    use super::{write_report_header, write_sample_report};

    #[test]
    fn writes_hybrid_lifecycle_wording_in_chinese() -> io::Result<()> {
        let settings = BenchmarkSettings {
            upstream: "https://example.invalid/v1".to_owned(),
            model: "test-model".to_owned(),
            prompt: "test prompt".to_owned(),
            workload_source: WorkloadSource::BuiltIn,
            runs: 12,
            warmups: 1,
            timeout: Duration::from_secs(1),
        };
        let mut output = Vec::new();

        write_report_header(&mut output, &settings)?;

        let output = String::from_utf8(output).map_err(io::Error::other)?;
        assert!(output.contains(
            "Hybrid 生命周期：冷启动 HTTP 单独取证；正式样本等待池 ready 后全部使用 WS。"
        ));
        Ok(())
    }

    #[test]
    fn reports_hybrid_http_and_websocket_counts_per_sample() -> io::Result<()> {
        let sample = Sample {
            e2e: Duration::from_millis(80),
            setup: Duration::from_millis(20),
            raw_bytes: 10,
            encoded_bytes: 8,
            logical_requests: 5,
            application_messages: 5,
            http_requests: 2,
            websocket_messages: 3,
            response_events: 5,
            websocket_handshakes: 1,
            round_e2e: vec![
                Duration::from_millis(100),
                Duration::from_millis(200),
                Duration::from_millis(300),
                Duration::from_millis(400),
                Duration::from_millis(500),
            ],
            first_events: vec![
                Duration::from_millis(10),
                Duration::from_millis(20),
                Duration::from_millis(30),
                Duration::from_millis(40),
                Duration::from_millis(50),
            ],
            warm_round_e2e: vec![
                Duration::from_millis(200),
                Duration::from_millis(300),
                Duration::from_millis(400),
                Duration::from_millis(500),
            ],
            connection_lifetime: Some(Duration::from_millis(70)),
            websocket_reconnects: 0,
            messages_per_connection: Some(3),
            retries: 1,
            round_transports: vec![
                crate::benchmark::RoundTransport::Http,
                crate::benchmark::RoundTransport::WebSocket,
                crate::benchmark::RoundTransport::WebSocket,
                crate::benchmark::RoundTransport::Http,
                crate::benchmark::RoundTransport::WebSocket,
            ],
        };
        let mut output = Vec::new();

        write_sample_report(&mut output, 0, &sample)?;

        let output = String::from_utf8_lossy(&output);
        assert!(output.contains("retries=1 transports=HTTP→WS→WS→HTTP→WS"));
        assert!(output.contains("http_requests=2 websocket_messages=3"));
        assert!(output.contains("http_ttft=25.0[10.0,40.0]ms"));
        assert!(output.contains("ws_ttft=30.0[20.0,50.0]ms"));
        assert!(output.contains("http_complete=250.0[100.0,400.0]ms"));
        assert!(output.contains("ws_complete=300.0[200.0,500.0]ms"));
        Ok(())
    }
}
