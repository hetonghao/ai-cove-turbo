use std::{
    collections::VecDeque,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::Path,
    sync::{Mutex, MutexGuard},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

mod window;

const BASE_BUCKET_MS: u64 = 10_000;
const RETENTION_MS: u64 = 25 * 60 * 60 * 1_000;
const RECENT_REQUEST_LIMIT: usize = 100;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub(crate) enum TrafficTransport {
    Http,
    Ws,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum TrafficResult {
    Success,
    Fallback,
    Error,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum TrafficRoute {
    HybridWs,
    HybridColdStartHttp,
    HybridRecoveryHttp,
    DirectHttp,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TrafficRouteCounts {
    pub(crate) hybrid_ws: u64,
    pub(crate) hybrid_cold_start_http: u64,
    pub(crate) hybrid_recovery_http: u64,
    pub(crate) direct_http: u64,
}

impl TrafficRouteCounts {
    const fn record(&mut self, route: TrafficRoute) {
        let counter = match route {
            TrafficRoute::HybridWs => &mut self.hybrid_ws,
            TrafficRoute::HybridColdStartHttp => &mut self.hybrid_cold_start_http,
            TrafficRoute::HybridRecoveryHttp => &mut self.hybrid_recovery_http,
            TrafficRoute::DirectHttp => &mut self.direct_http,
        };
        *counter = counter.saturating_add(1);
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum FailurePhase {
    HybridIdle,
    HybridActive,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RequestEvent {
    pub(crate) id: u64,
    timestamp_ms: u64,
    status: u16,
    path: String,
    raw_bytes: u64,
    sent_bytes: u64,
    transport: TrafficTransport,
    result: TrafficResult,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    route: Option<TrafficRoute>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    failure_phase: Option<FailurePhase>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    failure_reason: Option<String>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct TrafficRecord<'a> {
    pub(crate) timestamp_ms: u64,
    pub(crate) status: u16,
    pub(crate) path: &'a str,
    pub(crate) raw_bytes: u64,
    pub(crate) sent_bytes: u64,
    pub(crate) transport: TrafficTransport,
    pub(crate) result: TrafficResult,
    pub(crate) route: Option<TrafficRoute>,
    pub(crate) failure_phase: Option<FailurePhase>,
    pub(crate) failure_reason: Option<&'a str>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TrafficTotals {
    requests: u64,
    raw_bytes: u64,
    sent_bytes: u64,
}

impl TrafficTotals {
    const fn add_record(&mut self, record: TrafficRecord<'_>) {
        self.requests += 1;
        self.raw_bytes = self.raw_bytes.saturating_add(record.raw_bytes);
        self.sent_bytes = self.sent_bytes.saturating_add(record.sent_bytes);
    }

    const fn add_totals(&mut self, totals: Self) {
        self.requests = self.requests.saturating_add(totals.requests);
        self.raw_bytes = self.raw_bytes.saturating_add(totals.raw_bytes);
        self.sent_bytes = self.sent_bytes.saturating_add(totals.sent_bytes);
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TrafficSeries {
    transport: TrafficTransport,
    result: TrafficResult,
    requests: u64,
    raw_bytes: u64,
    sent_bytes: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TrafficBucket {
    pub(crate) start_ms: u64,
    pub(crate) end_ms: u64,
    series: Vec<TrafficSeries>,
}

impl TrafficBucket {
    #[cfg(test)]
    fn requests(&self) -> u64 {
        self.series.iter().map(|series| series.requests).sum()
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TrafficWindow {
    pub(crate) minutes: u16,
    bucket_seconds: u16,
    current_period_start_ms: u64,
    pub(crate) buckets: Vec<TrafficBucket>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
struct BaseBucket {
    start_ms: u64,
    totals: [TrafficTotals; 6],
}

#[derive(Debug, Default)]
struct TrafficState {
    latest_timestamp_ms: u64,
    next_id: u64,
    recent_requests: VecDeque<RequestEvent>,
    buckets: VecDeque<BaseBucket>,
    route_counts: TrafficRouteCounts,
    dirty_from_ms: Option<u64>,
    window_cache: Option<WindowCache>,
}

#[derive(Debug)]
struct WindowCache {
    period_start_ms: u64,
    windows: Vec<TrafficWindow>,
}

#[derive(Clone, Debug)]
pub(crate) struct TrafficSnapshot {
    pub(crate) recent_requests: Vec<RequestEvent>,
    pub(crate) windows: Vec<TrafficWindow>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct TrafficDelta {
    latest_timestamp_ms: u64,
    next_id: u64,
    recent_requests: Vec<RequestEvent>,
    buckets: Vec<BaseBucket>,
    #[serde(default)]
    route_counts: TrafficRouteCounts,
}

#[derive(Debug, Default)]
pub(crate) struct TrafficStore {
    state: Mutex<TrafficState>,
}

impl TrafficStore {
    pub(crate) fn load(path: &Path) -> Self {
        Self::load_at(path, now_ms())
    }

    fn load_at(path: &Path, now_ms: u64) -> Self {
        let mut state = TrafficState::default();
        for line in fs::read(path)
            .unwrap_or_default()
            .split(|byte| *byte == b'\n')
        {
            if let Ok(delta) = serde_json::from_slice(line) {
                state.apply(delta);
            }
        }
        state.trim(now_ms);
        Self {
            state: Mutex::new(state),
        }
    }

    pub(crate) fn save(&self, path: &Path) -> io::Result<()> {
        let (dirty_from_ms, delta) = {
            let mut state = lock(&self.state);
            let Some(dirty_from_ms) = state.dirty_from_ms.take() else {
                return Ok(());
            };
            (dirty_from_ms, state.delta_from(dirty_from_ms))
        };
        let result = (|| {
            let parent = path
                .parent()
                .ok_or_else(|| io::Error::other("traffic path has no parent"))?;
            fs::create_dir_all(parent)?;
            let mut file = OpenOptions::new().create(true).append(true).open(path)?;
            file.write_all(b"\n")?;
            serde_json::to_writer(&mut file, &delta).map_err(io::Error::other)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            lock(&self.state).mark_dirty(dirty_from_ms);
        }
        result
    }

    pub(crate) fn compact(&self, path: &Path) -> io::Result<()> {
        self.compact_at(path, now_ms())
    }

    fn compact_at(&self, path: &Path, now_ms: u64) -> io::Result<()> {
        let (pending_from_ms, delta) = {
            let mut state = lock(&self.state);
            state.trim(now_ms);
            let pending_from_ms = state.dirty_from_ms.take();
            (pending_from_ms, state.delta_from(0))
        };
        let result = (|| {
            let parent = path
                .parent()
                .ok_or_else(|| io::Error::other("traffic path has no parent"))?;
            fs::create_dir_all(parent)?;
            let mut temporary = NamedTempFile::new_in(parent)?;
            serde_json::to_writer(&mut temporary, &delta).map_err(io::Error::other)?;
            temporary.write_all(b"\n")?;
            temporary.as_file().sync_all()?;
            temporary.persist(path).map_err(|error| error.error)?;
            Ok(())
        })();
        if let (Err(_), Some(dirty_from_ms)) = (&result, pending_from_ms) {
            lock(&self.state).mark_dirty(dirty_from_ms);
        }
        result
    }

    pub(crate) fn record(&self, record: TrafficRecord<'_>) {
        if record.failure_phase == Some(FailurePhase::HybridIdle) && record.status != 1012 {
            return;
        }
        let event = RequestEvent {
            id: 0,
            timestamp_ms: record.timestamp_ms,
            status: record.status,
            path: record.path.to_owned(),
            raw_bytes: record.raw_bytes,
            sent_bytes: record.sent_bytes,
            transport: record.transport,
            result: record.result,
            route: record.route,
            failure_phase: record.failure_phase,
            failure_reason: record.failure_reason.map(str::to_owned),
        };
        let mut state = lock(&self.state);
        state.next_id = state.next_id.saturating_add(1);
        let id = state.next_id;
        state
            .recent_requests
            .push_back(RequestEvent { id, ..event });
        if state.recent_requests.len() > RECENT_REQUEST_LIMIT {
            state.recent_requests.pop_front();
        }

        let bucket_start = align(record.timestamp_ms, BASE_BUCKET_MS);
        let index = match state
            .buckets
            .back()
            .map(|bucket| bucket.start_ms.cmp(&bucket_start))
        {
            None | Some(std::cmp::Ordering::Less) => {
                state.buckets.push_back(BaseBucket {
                    start_ms: bucket_start,
                    ..BaseBucket::default()
                });
                state.buckets.len().saturating_sub(1)
            }
            Some(std::cmp::Ordering::Equal) => state.buckets.len().saturating_sub(1),
            Some(std::cmp::Ordering::Greater) => {
                let index = state
                    .buckets
                    .iter()
                    .position(|bucket| bucket.start_ms >= bucket_start)
                    .unwrap_or(state.buckets.len());
                if state
                    .buckets
                    .get(index)
                    .is_none_or(|bucket| bucket.start_ms != bucket_start)
                {
                    state.buckets.insert(
                        index,
                        BaseBucket {
                            start_ms: bucket_start,
                            ..BaseBucket::default()
                        },
                    );
                }
                index
            }
        };
        if record.failure_phase != Some(FailurePhase::HybridIdle) {
            if let Some(route) = record.route {
                state.route_counts.record(route);
            }
            if let Some(totals) = state.buckets.get_mut(index).and_then(|bucket| {
                bucket
                    .totals
                    .get_mut(class_index(record.transport, record.result))
            }) {
                totals.add_record(record);
            }
        }

        state.latest_timestamp_ms = state.latest_timestamp_ms.max(record.timestamp_ms);
        let latest_timestamp_ms = state.latest_timestamp_ms;
        state.mark_dirty(bucket_start);
        state.window_cache = None;
        state.trim(latest_timestamp_ms);
    }

    pub(crate) fn route_counts(&self) -> TrafficRouteCounts {
        lock(&self.state).route_counts
    }

    pub(crate) fn snapshot(&self) -> TrafficSnapshot {
        self.snapshot_at(now_ms())
    }

    fn snapshot_at(&self, now_ms: u64) -> TrafficSnapshot {
        let mut state = lock(&self.state);
        let period_start_ms = align(now_ms, BASE_BUCKET_MS);
        let windows = match &state.window_cache {
            Some(cache) if cache.period_start_ms == period_start_ms => cache.windows.clone(),
            Some(_) | None => {
                let windows = window::build_windows(&state.buckets, now_ms);
                state.window_cache = Some(WindowCache {
                    period_start_ms,
                    windows: windows.clone(),
                });
                windows
            }
        };
        TrafficSnapshot {
            recent_requests: state.recent_requests.iter().cloned().collect(),
            windows,
        }
    }
}

impl TrafficState {
    fn apply(&mut self, delta: TrafficDelta) {
        self.latest_timestamp_ms = self.latest_timestamp_ms.max(delta.latest_timestamp_ms);
        self.next_id = self.next_id.max(delta.next_id);
        self.recent_requests = delta.recent_requests.into();
        self.route_counts = delta.route_counts;
        for bucket in delta.buckets {
            let index = self
                .buckets
                .iter()
                .position(|current| current.start_ms >= bucket.start_ms)
                .unwrap_or(self.buckets.len());
            if let Some(current) = self
                .buckets
                .get_mut(index)
                .filter(|current| current.start_ms == bucket.start_ms)
            {
                *current = bucket;
            } else {
                self.buckets.insert(index, bucket);
            }
        }
    }

    fn delta_from(&self, start_ms: u64) -> TrafficDelta {
        TrafficDelta {
            latest_timestamp_ms: self.latest_timestamp_ms,
            next_id: self.next_id,
            recent_requests: self.recent_requests.iter().cloned().collect(),
            route_counts: self.route_counts,
            buckets: self
                .buckets
                .iter()
                .filter(|bucket| bucket.start_ms >= start_ms)
                .copied()
                .collect(),
        }
    }

    fn mark_dirty(&mut self, bucket_start_ms: u64) {
        self.dirty_from_ms = Some(
            self.dirty_from_ms
                .map_or(bucket_start_ms, |current| current.min(bucket_start_ms)),
        );
    }

    fn trim(&mut self, now_ms: u64) {
        self.window_cache = None;
        let cutoff = now_ms.saturating_sub(RETENTION_MS);
        self.recent_requests
            .retain(|request| request.timestamp_ms >= cutoff);
        while self.recent_requests.len() > RECENT_REQUEST_LIMIT {
            self.recent_requests.pop_front();
        }
        while self
            .buckets
            .front()
            .is_some_and(|bucket| bucket.start_ms + BASE_BUCKET_MS <= cutoff)
        {
            self.buckets.pop_front();
        }
    }
}

pub(crate) fn now_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

const fn align(timestamp_ms: u64, bucket_ms: u64) -> u64 {
    timestamp_ms - timestamp_ms % bucket_ms
}

const fn class_index(transport: TrafficTransport, result: TrafficResult) -> usize {
    match (transport, result) {
        (TrafficTransport::Http, TrafficResult::Success) => 0,
        (TrafficTransport::Http, TrafficResult::Fallback) => 1,
        (TrafficTransport::Http, TrafficResult::Error) => 2,
        (TrafficTransport::Ws, TrafficResult::Success) => 3,
        (TrafficTransport::Ws, TrafficResult::Fallback) => 4,
        (TrafficTransport::Ws, TrafficResult::Error) => 5,
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests;
