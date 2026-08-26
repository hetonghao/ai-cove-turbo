use std::{
    mem,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
    time::Instant,
};

use axum::body::Bytes;
use futures_util::{Stream, StreamExt, TryStreamExt};

use super::{HttpRequestMetric, HttpTraffic, Metrics, is_first_output_event_type};

const REQUEST_CANCELLED_STATUS: u16 = 499;

#[derive(Default)]
pub(super) struct HttpTimingControl {
    cancelled: AtomicBool,
    stream_failure: AtomicU8,
    recorded: AtomicBool,
}

impl HttpTimingControl {
    pub(super) fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub(super) fn fail_stream(&self) {
        self.stream_failure.store(1, Ordering::Release);
    }

    pub(super) fn fail_stream_error(&self) {
        self.stream_failure.store(2, Ordering::Release);
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    fn stream_failure(&self) -> u8 {
        self.stream_failure.load(Ordering::Acquire)
    }

    pub(super) fn claim_recording(&self) -> bool {
        self.recorded
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
}

pub(super) struct HttpTimingInput {
    pub(super) metrics: Arc<Metrics>,
    pub(super) started_at: Instant,
    pub(super) path: String,
    pub(super) status: u16,
    pub(super) raw_bytes: u64,
    pub(super) sent_bytes: u64,
    pub(super) compressed: bool,
    pub(super) traffic: HttpTraffic,
    pub(super) failure_reason: Option<String>,
    pub(super) control: Option<Arc<HttpTimingControl>>,
}

pub(super) struct HttpTiming {
    input: HttpTimingInput,
    pending: Vec<u8>,
    data: Vec<u8>,
    first_token_at: Option<Instant>,
    recorded: bool,
}

impl HttpTiming {
    pub(super) const fn new(input: HttpTimingInput) -> Self {
        Self {
            input,
            pending: Vec::new(),
            data: Vec::new(),
            first_token_at: None,
            recorded: false,
        }
    }

    pub(super) fn observe(&mut self, chunk: &[u8]) {
        if self.first_token_at.is_some() || !super::is_responses_path(&self.input.path) {
            return;
        }
        self.pending.extend_from_slice(chunk);
        while let Some(newline) = self.pending.iter().position(|byte| *byte == b'\n') {
            let mut line = self.pending.drain(..=newline).collect::<Vec<_>>();
            let _ = line.pop();
            if line.last() == Some(&b'\r') {
                let _ = line.pop();
            }
            self.observe_line(&line);
            if self.first_token_at.is_some() {
                return;
            }
        }
    }

    fn observe_line(&mut self, line: &[u8]) {
        if line.is_empty() {
            self.observe_event();
            return;
        }
        let Some(data) = line.strip_prefix(b"data:") else {
            return;
        };
        let data = data.strip_prefix(b" ").unwrap_or(data);
        if !self.data.is_empty() {
            self.data.push(b'\n');
        }
        self.data.extend_from_slice(data);
    }

    fn observe_event(&mut self) {
        let data = mem::take(&mut self.data);
        if data.is_empty() {
            return;
        }
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&data) else {
            return;
        };
        let Some(event_type) = value.get("type").and_then(serde_json::Value::as_str) else {
            return;
        };
        if is_first_output_event_type(event_type) {
            self.first_token_at = Some(Instant::now());
        }
    }

    pub(super) fn finish(&mut self) {
        if let Some(control) = &self.input.control {
            if control.is_cancelled() {
                self.finish_with(
                    REQUEST_CANCELLED_STATUS,
                    Some("request cancelled by client"),
                );
                return;
            }
            match control.stream_failure() {
                1 => {
                    self.finish_with(
                        502,
                        Some("HTTP stream ended before terminal response event"),
                    );
                    return;
                }
                2 => {
                    self.finish_with(502, Some("HTTP response stream failed"));
                    return;
                }
                _ => {}
            }
        }
        let failure_reason = self.input.failure_reason.clone();
        self.finish_with(self.input.status, failure_reason.as_deref());
    }

    pub(super) fn finish_stream_error(&mut self) {
        if let Some(control) = &self.input.control {
            control.fail_stream_error();
            self.finish();
        } else {
            self.finish_with(502, Some("HTTP response stream failed"));
        }
    }

    pub(super) fn finish_stream_end(&mut self) {
        if self.input.control.is_some() {
            self.finish_with(
                502,
                Some("HTTP stream ended before terminal response event"),
            );
        } else {
            self.finish();
        }
    }

    fn finish_with(&mut self, status: u16, failure_reason: Option<&str>) {
        if self.recorded {
            return;
        }
        self.recorded = true;
        if let Some(control) = &self.input.control {
            if !control.claim_recording() {
                return;
            }
        }
        let duration_ms = Some(elapsed_ms(self.input.started_at, Instant::now()));
        let first_token_ms = self
            .first_token_at
            .map(|first_token_at| elapsed_ms(self.input.started_at, first_token_at));
        self.input.metrics.record_http_with_timing(
            HttpRequestMetric {
                path: &self.input.path,
                status,
                raw_bytes: usize::try_from(self.input.raw_bytes).unwrap_or(usize::MAX),
                sent_bytes: usize::try_from(self.input.sent_bytes).unwrap_or(usize::MAX),
                compressed: self.input.compressed,
                result: self.input.traffic.result,
                route: self.input.traffic.route,
                failure_reason,
            },
            first_token_ms,
            duration_ms,
        );
    }
}

impl Drop for HttpTiming {
    fn drop(&mut self) {
        self.finish();
    }
}

pub(super) fn elapsed_ms(started_at: Instant, finished_at: Instant) -> u64 {
    u64::try_from(finished_at.duration_since(started_at).as_millis()).unwrap_or(u64::MAX)
}

pub(super) fn instrument_http_stream<S>(
    stream: S,
    timing: HttpTiming,
) -> impl Stream<Item = Result<Bytes, std::io::Error>> + Send
where
    S: Stream<Item = Result<Bytes, reqwest::Error>> + Send + 'static,
{
    let stream = stream.map_err(std::io::Error::other);
    futures_util::stream::unfold(
        (Box::pin(stream), timing),
        |(mut stream, mut timing)| async move {
            match stream.as_mut().next().await {
                Some(Ok(chunk)) => {
                    timing.observe(&chunk);
                    Some((Ok(chunk), (stream, timing)))
                }
                Some(Err(error)) => {
                    timing.finish_stream_error();
                    Some((Err(error), (stream, timing)))
                }
                None => {
                    timing.finish_stream_end();
                    None
                }
            }
        },
    )
}
