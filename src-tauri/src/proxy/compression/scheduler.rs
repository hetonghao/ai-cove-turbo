use std::{
    sync::{Arc, OnceLock},
    time::Instant,
};

use bytes::Bytes;
use tokio::{sync::Semaphore, task::JoinError};

use super::{CompressionMetricsSnapshot, metrics, metrics::CompressionMetrics};
use crate::proxy::{MIN_COMPRESSION_INPUT_BYTES, private_websocket};

const DEFAULT_COMPRESSION_CONCURRENCY: usize = 4;
const ZSTD_LEVEL: i32 = 3;

#[derive(Clone, Debug)]
pub(crate) struct CompressionScheduler {
    permits: Arc<Semaphore>,
    metrics: Arc<CompressionMetrics>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CompressionScheduleError;

impl CompressionScheduler {
    pub(crate) fn shared() -> Self {
        static SHARED: OnceLock<CompressionScheduler> = OnceLock::new();
        SHARED
            .get_or_init(|| Self::with_capacity(DEFAULT_COMPRESSION_CONCURRENCY))
            .clone()
    }

    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(capacity)),
            metrics: Arc::new(CompressionMetrics::default()),
        }
    }

    pub(crate) fn metrics_snapshot(&self) -> CompressionMetricsSnapshot {
        metrics::snapshot(&self.metrics)
    }

    #[cfg(test)]
    pub(crate) fn available_permits(&self) -> usize {
        self.permits.available_permits()
    }

    pub(crate) async fn run<T, F>(&self, work: F) -> Result<T, CompressionScheduleError>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let queued_at = Instant::now();
        let permit = self
            .permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| CompressionScheduleError)?;
        self.metrics.queue_wait_ms.fetch_add(
            metrics::elapsed_ms(queued_at.elapsed()),
            std::sync::atomic::Ordering::Relaxed,
        );
        let metrics = Arc::clone(&self.metrics);
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let started_at = Instant::now();
            let result = work();
            metrics.work_time_ms.fetch_add(
                metrics::elapsed_ms(started_at.elapsed()),
                std::sync::atomic::Ordering::Relaxed,
            );
            result
        })
        .await
        .map_err(|_: JoinError| CompressionScheduleError)
    }

    #[allow(clippy::single_match_else, clippy::option_if_let_else)]
    pub(crate) async fn encode_http(&self, body: Bytes) -> Result<Option<Bytes>, ()> {
        self.metrics
            .encode_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if body.len() < MIN_COMPRESSION_INPUT_BYTES {
            self.metrics
                .fast_path_count
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return Ok(None);
        }
        let original_len = body.len();
        let result = self
            .run(move || {
                zstd::stream::encode_all(std::io::Cursor::new(body), ZSTD_LEVEL)
                    .map(Bytes::from)
                    .map(|compressed| (compressed.len() < original_len).then_some(compressed))
                    .map_err(|_| ())
            })
            .await;
        match result {
            Ok(result) => {
                if result.is_err() {
                    self.metrics
                        .failures
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                result
            }
            Err(_) => {
                self.metrics
                    .failures
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Err(())
            }
        }
    }

    #[allow(clippy::single_match_else, clippy::option_if_let_else)]
    pub(crate) async fn encode_private(
        &self,
        payload: Vec<u8>,
        original_binary: bool,
    ) -> Result<private_websocket::EncodedPrivateMessage, private_websocket::PrivateProtocolError>
    {
        self.metrics
            .encode_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if !private_websocket::should_offload_private_encoding(payload.len()) {
            self.metrics
                .fast_path_count
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let result =
                private_websocket::encode_private_message_with_metadata(&payload, original_binary);
            if result.is_err() {
                self.metrics
                    .failures
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            return result;
        }
        let result = self
            .run(move || {
                private_websocket::encode_private_message_with_metadata(&payload, original_binary)
            })
            .await;
        match result {
            Ok(result) => {
                if result.is_err() {
                    self.metrics
                        .failures
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                result
            }
            Err(_) => {
                self.metrics
                    .failures
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Err(private_websocket::PrivateProtocolError::internal(
                    "private websocket worker failed",
                ))
            }
        }
    }

    #[allow(clippy::single_match_else, clippy::option_if_let_else)]
    pub(crate) async fn decode_private(
        &self,
        envelope: Bytes,
    ) -> Result<private_websocket::DecodedPrivateMessage, private_websocket::PrivateProtocolError>
    {
        self.metrics
            .decode_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let result = self
            .run(move || private_websocket::decode_private_message(&envelope))
            .await;
        match result {
            Ok(result) => {
                if result.is_err() {
                    self.metrics
                        .failures
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                result
            }
            Err(_) => {
                self.metrics
                    .failures
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Err(private_websocket::PrivateProtocolError::internal(
                    "private websocket worker failed",
                ))
            }
        }
    }
}
