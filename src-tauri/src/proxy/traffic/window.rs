use std::collections::VecDeque;

use super::{
    BaseBucket, TrafficBucket, TrafficResult, TrafficSeries, TrafficTotals, TrafficTransport,
    TrafficWindow, align,
};

const WINDOW_SPECS: [(u16, u64, usize); 4] = [
    (1, 10_000, 6),
    (10, 60_000, 10),
    (60, 5 * 60_000, 12),
    (1_440, 60 * 60_000, 24),
];

pub(super) fn build_windows(
    base_buckets: &VecDeque<BaseBucket>,
    now_ms: u64,
) -> Vec<TrafficWindow> {
    WINDOW_SPECS
        .iter()
        .map(|&(minutes, bucket_ms, bucket_count)| {
            build_window(base_buckets, now_ms, minutes, bucket_ms, bucket_count)
        })
        .collect()
}

fn build_window(
    base_buckets: &VecDeque<BaseBucket>,
    now_ms: u64,
    minutes: u16,
    bucket_ms: u64,
    bucket_count: usize,
) -> TrafficWindow {
    let current_period_start_ms = align(now_ms, bucket_ms);
    let range_start_ms = current_period_start_ms.saturating_sub(
        bucket_ms.saturating_mul(u64::try_from(bucket_count.saturating_sub(1)).unwrap_or_default()),
    );
    let mut totals = vec![[TrafficTotals::default(); 6]; bucket_count];
    for base in base_buckets {
        if base.start_ms < range_start_ms || base.start_ms >= current_period_start_ms + bucket_ms {
            continue;
        }
        let index = usize::try_from((base.start_ms - range_start_ms) / bucket_ms)
            .unwrap_or_else(|_| bucket_count.saturating_sub(1))
            .min(bucket_count.saturating_sub(1));
        let Some(bucket_totals) = totals.get_mut(index) else {
            continue;
        };
        for (target, source) in bucket_totals.iter_mut().zip(base.totals) {
            target.add_totals(source);
        }
    }
    let buckets = totals
        .into_iter()
        .enumerate()
        .map(|(index, totals)| {
            let start_ms =
                range_start_ms + bucket_ms.saturating_mul(u64::try_from(index).unwrap_or_default());
            TrafficBucket {
                start_ms,
                end_ms: start_ms + bucket_ms,
                series: totals
                    .into_iter()
                    .enumerate()
                    .filter(|(_, totals)| totals.requests > 0)
                    .map(|(index, totals)| {
                        let (transport, result) = class_values(index);
                        TrafficSeries {
                            transport,
                            result,
                            requests: totals.requests,
                            raw_bytes: totals.raw_bytes,
                            sent_bytes: totals.sent_bytes,
                        }
                    })
                    .collect(),
            }
        })
        .collect();
    TrafficWindow {
        minutes,
        bucket_seconds: u16::try_from(bucket_ms / 1_000).unwrap_or(u16::MAX),
        current_period_start_ms,
        buckets,
    }
}

const fn class_values(index: usize) -> (TrafficTransport, TrafficResult) {
    match index {
        0 => (TrafficTransport::Http, TrafficResult::Success),
        1 => (TrafficTransport::Http, TrafficResult::Fallback),
        2 => (TrafficTransport::Http, TrafficResult::Error),
        3 => (TrafficTransport::Ws, TrafficResult::Success),
        4 => (TrafficTransport::Ws, TrafficResult::Fallback),
        _ => (TrafficTransport::Ws, TrafficResult::Error),
    }
}
