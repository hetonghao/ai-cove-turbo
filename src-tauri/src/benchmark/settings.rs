use std::{env, fs, io, time::Duration};

use super::{BenchmarkResult, benchmark_error};

pub(super) const DEFAULT_MODEL: &str = "gpt-5.6-luna";
pub(super) const DEFAULT_MULTI_ROUNDS: usize = 5;
pub(super) const DEFAULT_TIMEOUT: Duration = Duration::from_secs(180);

pub(super) const DEFAULT_UPSTREAM: &str = "https://api.ai-cove.com/v1";
const DEFAULT_SHORT_PROMPT: &str = "Reply with OK only.";
const DEFAULT_LONG_PROMPT_CHARS: usize = 96 * 1024;
const DEFAULT_RUNS: usize = 8;
const DEFAULT_WARMUPS: usize = 1;

#[derive(Debug)]
pub(super) struct BenchmarkSettings {
    pub(super) upstream: String,
    pub(super) model: String,
    pub(super) prompt: String,
    pub(super) workload_source: WorkloadSource,
    pub(super) runs: usize,
    pub(super) warmups: usize,
    pub(super) timeout: Duration,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum WorkloadSource {
    BuiltIn,
    InlineEnvironment,
    File,
}

impl WorkloadSource {
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::BuiltIn => "builtin-source-smoke",
            Self::InlineEnvironment => "inline-environment",
            Self::File => "fixed-file",
        }
    }
}

#[derive(Debug)]
pub(super) struct UsageScenario {
    pub(super) name: &'static str,
    pub(super) prompts: Vec<String>,
    pub(super) requires_compression: bool,
}

impl BenchmarkSettings {
    pub(super) fn from_env() -> BenchmarkResult<Self> {
        let (prompt, workload_source) = benchmark_prompt_from_env()?;
        Ok(Self {
            upstream: env::var("TURBO_BENCHMARK_UPSTREAM")
                .unwrap_or_else(|_| DEFAULT_UPSTREAM.to_owned()),
            model: env::var("TURBO_BENCHMARK_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_owned()),
            prompt,
            workload_source,
            runs: validate_runs(positive_env("TURBO_BENCHMARK_RUNS", DEFAULT_RUNS)?)?,
            warmups: non_negative_env("TURBO_BENCHMARK_WARMUPS", DEFAULT_WARMUPS)?,
            timeout: Duration::from_secs(positive_env_u64(
                "TURBO_BENCHMARK_TIMEOUT_SECS",
                DEFAULT_TIMEOUT.as_secs(),
            )?),
        })
    }
}

pub(super) fn validate_runs(runs: usize) -> Result<usize, io::Error> {
    if runs == 0 || !runs.is_multiple_of(4) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "TURBO_BENCHMARK_RUNS must be a positive multiple of 4",
        ));
    }
    Ok(runs)
}

fn benchmark_prompt_from_env() -> BenchmarkResult<(String, WorkloadSource)> {
    let inline = optional_env("TURBO_BENCHMARK_PROMPT")?;
    let file = optional_env("TURBO_BENCHMARK_PROMPT_FILE")?;
    match (inline, file) {
        (Some(_), Some(_)) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "set only one of TURBO_BENCHMARK_PROMPT or TURBO_BENCHMARK_PROMPT_FILE",
        )),
        (Some(prompt), None) => Ok((prompt, WorkloadSource::InlineEnvironment)),
        (None, Some(path)) => Ok((fs::read_to_string(path)?, WorkloadSource::File)),
        (None, None) => Ok((default_long_prompt(), WorkloadSource::BuiltIn)),
    }
}

fn optional_env(name: &str) -> BenchmarkResult<Option<String>> {
    match env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(error) => Err(benchmark_error(error)),
    }
}

pub(super) fn default_long_prompt() -> String {
    include_str!("../../Cargo.lock")
        .chars()
        .take(DEFAULT_LONG_PROMPT_CHARS)
        .collect()
}

pub(super) fn workload_fingerprint(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn positive_env(name: &str, default: usize) -> BenchmarkResult<usize> {
    let value = env::var(name).unwrap_or_else(|_| default.to_string());
    let parsed = value.parse::<usize>().map_err(benchmark_error)?;
    if parsed == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{name} must be positive"),
        ));
    }
    Ok(parsed)
}

fn positive_env_u64(name: &str, default: u64) -> BenchmarkResult<u64> {
    let value = env::var(name).unwrap_or_else(|_| default.to_string());
    let parsed = value.parse::<u64>().map_err(benchmark_error)?;
    if parsed == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{name} must be positive"),
        ));
    }
    Ok(parsed)
}

fn non_negative_env(name: &str, default: usize) -> BenchmarkResult<usize> {
    env::var(name)
        .unwrap_or_else(|_| default.to_string())
        .parse::<usize>()
        .map_err(benchmark_error)
}

pub(super) fn usage_scenarios(settings: &BenchmarkSettings) -> Vec<UsageScenario> {
    let multi_turn_prompts = std::iter::once(settings.prompt.clone())
        .chain((2..=DEFAULT_MULTI_ROUNDS).map(|round| {
            format!("Turbo benchmark continuation round {round}; reply with OK only.")
        }))
        .collect();
    vec![
        UsageScenario {
            name: "单轮短上下文",
            prompts: vec![DEFAULT_SHORT_PROMPT.to_owned()],
            requires_compression: false,
        },
        UsageScenario {
            name: "单轮长上下文",
            prompts: vec![settings.prompt.clone()],
            requires_compression: true,
        },
        UsageScenario {
            name: "连续 5 轮真实 continuation",
            prompts: multi_turn_prompts,
            requires_compression: true,
        },
    ]
}
