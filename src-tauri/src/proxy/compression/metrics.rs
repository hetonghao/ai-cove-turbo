use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

#[derive(Debug, Default)]
pub(super) struct CompressionMetrics {
    pub(super) encode_count: AtomicU64,
    pub(super) decode_count: AtomicU64,
    pub(super) queue_wait_ms: AtomicU64,
    pub(super) work_time_ms: AtomicU64,
    pub(super) failures: AtomicU64,
    pub(super) fast_path_count: AtomicU64,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CompressionMetricsSnapshot {
    pub(crate) encode_count: u64,
    pub(crate) decode_count: u64,
    pub(crate) queue_wait_ms: u64,
    pub(crate) work_time_ms: u64,
    pub(crate) failures: u64,
    pub(crate) fast_path_count: u64,
}

pub(super) fn elapsed_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

pub(super) fn snapshot(metrics: &CompressionMetrics) -> CompressionMetricsSnapshot {
    CompressionMetricsSnapshot {
        encode_count: metrics.encode_count.load(Ordering::Relaxed),
        decode_count: metrics.decode_count.load(Ordering::Relaxed),
        queue_wait_ms: metrics.queue_wait_ms.load(Ordering::Relaxed),
        work_time_ms: metrics.work_time_ms.load(Ordering::Relaxed),
        failures: metrics.failures.load(Ordering::Relaxed),
        fast_path_count: metrics.fast_path_count.load(Ordering::Relaxed),
    }
}
