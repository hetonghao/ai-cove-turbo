use std::{env, io, process::Command};

use super::super::super::{BenchmarkResult, BenchmarkSettings, workload_fingerprint};
use super::types::{ArtifactMetadata, FixtureMetadata, StrategyConstants};

const MAX_OUTPUT_TOKENS: u64 = 16;
const COMPRESSION_LEVEL: u8 = 3;
const MIN_COMPRESSION_INPUT_BYTES: u64 = 1_024;
const MAX_REQUEST_BODY_BYTES: u64 = 128 * 1024 * 1024;
const REFERENCE_UPLINK_MBPS: f64 = 10.0;

pub(super) fn metadata(settings: &BenchmarkSettings) -> ArtifactMetadata {
    ArtifactMetadata {
        turbo_sha: env_or_command("TURBO_GIT_SHA", "git", &["rev-parse", "HEAD"]),
        rust_toolchain: env_or_command("TURBO_RUST_TOOLCHAIN", "rustc", &["--version"]),
        target_platform: env::var("TURBO_TARGET_PLATFORM")
            .unwrap_or_else(|_| format!("{}-{}", env::consts::OS, env::consts::ARCH)),
        cargo_profile: env::var("TURBO_CARGO_PROFILE").unwrap_or_else(|_| {
            if cfg!(test) {
                "test".to_owned()
            } else if cfg!(debug_assertions) {
                "debug".to_owned()
            } else {
                "release".to_owned()
            }
        }),
        model: settings.model.clone(),
        runs: settings.runs,
        warmups: settings.warmups,
    }
}

fn env_or_command(name: &str, program: &str, args: &[&str]) -> String {
    if let Ok(value) = env::var(name) {
        return value;
    }
    Command::new(program)
        .args(args)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "unknown".to_owned())
}

pub(super) fn fixture(settings: &BenchmarkSettings) -> BenchmarkResult<FixtureMetadata> {
    if settings.prompt.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "benchmark artifact fixture must not be empty",
        ));
    }
    Ok(FixtureMetadata {
        fingerprint: format!("{:016x}", workload_fingerprint(settings.prompt.as_bytes())),
        source: settings.workload_source.label().to_owned(),
        bytes: settings.prompt.len(),
    })
}

pub(super) const fn strategy_constants() -> StrategyConstants {
    StrategyConstants {
        max_output_tokens: MAX_OUTPUT_TOKENS,
        compression_level: COMPRESSION_LEVEL,
        min_compression_input_bytes: MIN_COMPRESSION_INPUT_BYTES,
        max_request_body_bytes: MAX_REQUEST_BODY_BYTES,
        reference_uplink_mbps: REFERENCE_UPLINK_MBPS,
    }
}
