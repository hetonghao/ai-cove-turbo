use super::*;

fn record(timestamp_ms: u64, raw_bytes: u64) -> TrafficRecord<'static> {
    TrafficRecord {
        timestamp_ms,
        status: 200,
        path: "/v1/responses",
        raw_bytes,
        sent_bytes: raw_bytes / 2,
        transport: TrafficTransport::Http,
        result: TrafficResult::Success,
    }
}

#[test]
fn rolling_windows_keep_current_period_on_the_right() {
    let store = TrafficStore::default();
    store.record(record(21_000, 100));
    store.record(record(71_000, 200));

    let first = store.windows_at(71_000);
    let minute = first.iter().find(|window| window.minutes == 1).unwrap();
    assert_eq!(minute.buckets.len(), 6);
    assert_eq!(minute.buckets.first().unwrap().start_ms, 20_000);
    assert_eq!(minute.buckets.last().unwrap().start_ms, 70_000);
    assert_eq!(minute.buckets.last().unwrap().end_ms, 80_000);
    assert_eq!(
        minute
            .buckets
            .iter()
            .map(TrafficBucket::requests)
            .sum::<u64>(),
        2
    );

    store.record(record(81_000, 300));
    let second = store.windows_at(81_000);
    let minute = second.iter().find(|window| window.minutes == 1).unwrap();
    assert_eq!(minute.buckets.first().unwrap().start_ms, 30_000);
    assert_eq!(minute.buckets.last().unwrap().start_ms, 80_000);
    assert_eq!(
        minute
            .buckets
            .iter()
            .map(TrafficBucket::requests)
            .sum::<u64>(),
        2
    );
    assert_eq!(minute.buckets.last().unwrap().requests(), 1);
}

#[test]
fn recent_requests_keep_only_the_latest_hundred() {
    let store = TrafficStore::default();
    for index in 0..101 {
        store.record(record(index * 1_000, 100));
    }

    let recent = store.recent_requests();
    assert_eq!(recent.len(), 100);
    assert_eq!(recent.first().unwrap().id, 2);
    assert_eq!(recent.last().unwrap().id, 101);
}
