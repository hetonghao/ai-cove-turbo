use std::{
    collections::VecDeque,
    sync::{Mutex, MutexGuard},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Serialize;

mod window;

const BASE_BUCKET_MS: u64 = 10_000;
const RETENTION_MS: u64 = 24 * 60 * 60 * 1_000;
const RECENT_REQUEST_LIMIT: usize = 100;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub(crate) enum TrafficTransport {
    Http,
    Ws,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum TrafficResult {
    Success,
    Fallback,
    Error,
}

#[derive(Clone, Debug, Serialize)]
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
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
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

#[derive(Clone, Copy, Debug, Default)]
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
}

#[derive(Debug, Default)]
pub(crate) struct TrafficStore {
    state: Mutex<TrafficState>,
}

impl TrafficStore {
    pub(crate) fn record(&self, record: TrafficRecord<'_>) {
        let event = RequestEvent {
            id: 0,
            timestamp_ms: record.timestamp_ms,
            status: record.status,
            path: record.path.to_owned(),
            raw_bytes: record.raw_bytes,
            sent_bytes: record.sent_bytes,
            transport: record.transport,
            result: record.result,
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
        let bucket_index = state
            .buckets
            .iter()
            .position(|bucket| bucket.start_ms >= bucket_start);
        let index = bucket_index.unwrap_or(state.buckets.len());
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
        if let Some(totals) = state.buckets.get_mut(index).and_then(|bucket| {
            bucket
                .totals
                .get_mut(class_index(record.transport, record.result))
        }) {
            totals.add_record(record);
        }

        state.latest_timestamp_ms = state.latest_timestamp_ms.max(record.timestamp_ms);
        let cutoff = state.latest_timestamp_ms.saturating_sub(RETENTION_MS);
        while state
            .buckets
            .front()
            .is_some_and(|bucket| bucket.start_ms + BASE_BUCKET_MS <= cutoff)
        {
            state.buckets.pop_front();
        }
        drop(state);
    }

    pub(crate) fn recent_requests(&self) -> Vec<RequestEvent> {
        lock(&self.state).recent_requests.iter().cloned().collect()
    }

    pub(crate) fn windows(&self) -> Vec<TrafficWindow> {
        self.windows_at(now_ms())
    }

    fn windows_at(&self, now_ms: u64) -> Vec<TrafficWindow> {
        let state = lock(&self.state);
        window::build_windows(&state.buckets, now_ms)
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
