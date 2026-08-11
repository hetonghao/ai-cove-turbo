use std::io;

use super::{BenchmarkPath, LiveContext, PayloadSet, rotated_paths};
use crate::benchmark::{
    BenchmarkCase, BenchmarkResult, UsageScenario, http_payload, websocket_payload,
};

pub(super) fn disable_path(disabled: &mut Vec<BenchmarkPath>, path: BenchmarkPath) {
    if !disabled.contains(&path) {
        disabled.push(path);
    }
}

pub(super) fn require_compression(case: &BenchmarkCase, required: bool) -> BenchmarkResult<()> {
    if !required {
        return Ok(());
    }
    let mut samples = case
        .samples
        .iter()
        .filter(|sample| sample.retries == 0)
        .peekable();
    if samples.peek().is_none() {
        return Err(io::Error::other(format!(
            "benchmark scenario={} path={} 没有无重试的有效样本",
            case.scenario, case.path
        )));
    }
    if samples.all(|sample| sample.encoded_bytes < sample.raw_bytes) {
        return Ok(());
    }
    Err(io::Error::other(format!(
        "{} / {} did not produce a smaller encoded payload",
        case.scenario, case.path
    )))
}

pub(super) fn sample_context_error(
    scenario: &str,
    path: BenchmarkPath,
    iteration: usize,
    error: &io::Error,
) -> io::Error {
    io::Error::other(format!(
        "benchmark scenario={scenario} path={} iteration={iteration} failed: {error}",
        path.label(),
    ))
}

pub(super) async fn collect_scenario(
    context: &LiveContext<'_>,
    scenario: UsageScenario,
    disabled: &mut Vec<BenchmarkPath>,
) -> BenchmarkResult<Vec<BenchmarkCase>> {
    let http_payloads = scenario
        .prompts
        .iter()
        .map(|prompt| http_payload(&context.settings.model, prompt))
        .collect::<Vec<_>>();
    let websocket_payloads = scenario
        .prompts
        .iter()
        .map(|prompt| websocket_payload(&context.settings.model, prompt))
        .collect::<Vec<_>>();
    let payloads = PayloadSet {
        http: &http_payloads,
        websocket: &websocket_payloads,
    };
    let mut cases = [
        BenchmarkPath::Direct,
        BenchmarkPath::Http,
        BenchmarkPath::WebSocket,
        BenchmarkPath::Hybrid,
    ]
    .map(|path| {
        (
            path,
            BenchmarkCase {
                scenario: scenario.name,
                path: path.label(),
                samples: Vec::with_capacity(context.settings.runs),
            },
        )
    });
    let iterations = context
        .settings
        .warmups
        .checked_add(context.settings.runs)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "too many benchmark runs"))?;
    for iteration in 0..iterations {
        for path in rotated_paths(iteration) {
            if disabled.contains(&path) {
                continue;
            }
            match context.collect_sample(path, &payloads).await {
                Ok(sample) if iteration >= context.settings.warmups => {
                    let case = cases
                        .iter_mut()
                        .find(|(candidate, _)| *candidate == path)
                        .ok_or_else(|| io::Error::other("benchmark path case is missing"))?;
                    case.1.samples.push(sample);
                }
                Ok(_) => {}
                Err(error) => {
                    let error = sample_context_error(scenario.name, path, iteration + 1, &error);
                    eprintln!("{error}; 后续跳过该路径，其他路径继续");
                    disable_path(disabled, path);
                }
            }
        }
    }
    let mut ready = Vec::with_capacity(cases.len());
    for (path, case) in cases {
        if case.samples.iter().all(|sample| sample.retries != 0) {
            continue;
        }
        let requires_compression = scenario.requires_compression && path != BenchmarkPath::Direct;
        if let Err(error) = require_compression(&case, requires_compression) {
            eprintln!("{error}; 后续跳过该路径，其他路径继续");
            disable_path(disabled, path);
            continue;
        }
        ready.push(case);
    }
    Ok(ready)
}

#[cfg(test)]
mod tests {
    use super::{BenchmarkPath, disable_path};

    #[test]
    fn disabling_one_failed_path_keeps_sibling_paths_available() {
        let mut disabled = Vec::new();

        disable_path(&mut disabled, BenchmarkPath::Hybrid);
        disable_path(&mut disabled, BenchmarkPath::Hybrid);

        assert_eq!(disabled, vec![BenchmarkPath::Hybrid]);
        assert!(!disabled.contains(&BenchmarkPath::Direct));
        assert!(!disabled.contains(&BenchmarkPath::Http));
        assert!(!disabled.contains(&BenchmarkPath::WebSocket));
    }
}
