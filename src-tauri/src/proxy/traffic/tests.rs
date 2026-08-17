use super::*;

use std::{error::Error, fs, io::Write};

use tempfile::tempdir;

fn record(timestamp_ms: u64, raw_bytes: u64) -> TrafficRecord<'static> {
    TrafficRecord {
        timestamp_ms,
        status: 200,
        path: "/v1/responses",
        raw_bytes,
        sent_bytes: raw_bytes / 2,
        transport: TrafficTransport::Http,
        result: TrafficResult::Success,
        route: None,
        failure_phase: None,
        failure_reason: None,
    }
}

#[test]
fn rolling_windows_keep_current_period_on_the_right() {
    let store = TrafficStore::default();
    store.record(record(21_000, 100));
    store.record(record(71_000, 200));

    let first = store.snapshot_at(71_000);
    assert_eq!(first.recent_requests.len(), 2);
    let minute = first
        .windows
        .iter()
        .find(|window| window.minutes == 1)
        .unwrap();
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
    let second = store.snapshot_at(81_000);
    let minute = second
        .windows
        .iter()
        .find(|window| window.minutes == 1)
        .unwrap();
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

    let idle = store.snapshot_at(91_000);
    let minute = idle
        .windows
        .iter()
        .find(|window| window.minutes == 1)
        .unwrap();
    assert_eq!(minute.buckets.last().unwrap().start_ms, 90_000);
}

#[test]
fn recent_requests_keep_only_the_latest_hundred() {
    let store = TrafficStore::default();
    for index in 0..101 {
        store.record(record(index * 1_000, 100));
    }

    let recent = store.snapshot_at(100_000).recent_requests;
    assert_eq!(recent.len(), 100);
    assert_eq!(recent.first().unwrap().id, 2);
    assert_eq!(recent.last().unwrap().id, 101);
}

#[test]
fn hybrid_idle_diagnostic_stays_internal_without_entering_recent_requests() {
    // Given: 一条发生在 Hybrid 空闲连接上的恢复诊断。
    let store = TrafficStore::default();
    let mut diagnostic = record(21_000, 0);
    diagnostic.status = 1002;
    diagnostic.transport = TrafficTransport::Ws;
    diagnostic.result = TrafficResult::Error;
    diagnostic.route = Some(TrafficRoute::HybridWs);
    diagnostic.failure_phase = Some(FailurePhase::HybridIdle);
    diagnostic.failure_reason = Some("unexpected idle upstream binary message");

    // When: 诊断进入持久化流量存储。
    store.record(diagnostic);
    let snapshot = store.snapshot_at(21_000);

    // Then: 空闲诊断不进入用户请求列表，统计窗口和路由计数也不增加。
    assert!(snapshot.recent_requests.is_empty());
    assert!(
        snapshot
            .windows
            .iter()
            .flat_map(|window| &window.buckets)
            .all(|bucket| bucket.requests() == 0)
    );
    assert_eq!(store.route_counts(), TrafficRouteCounts::default());
}

#[test]
fn persisted_traffic_round_trips() -> Result<(), Box<dyn Error>> {
    let root = tempdir()?;
    let path = root.path().join("traffic.jsonl");
    let store = TrafficStore::default();
    store.record(record(21_000, 100));
    store.save(&path)?;
    store.record(record(31_000, 200));
    store.save(&path)?;

    let restored = TrafficStore::load_at(&path, 31_000);

    let recent = restored.snapshot_at(31_000).recent_requests;
    assert_eq!(recent.len(), 2);
    let event = recent.first().unwrap();
    assert_eq!(event.id, 1);
    assert_eq!(event.raw_bytes, 100);
    assert_eq!(lock(&restored.state).buckets.len(), 2);
    assert_eq!(
        fs::read_to_string(path)?
            .lines()
            .filter(|line| !line.is_empty())
            .count(),
        2
    );
    Ok(())
}

#[test]
fn persisted_traffic_ignores_a_damaged_utf8_tail() -> Result<(), Box<dyn Error>> {
    let root = tempdir()?;
    let path = root.path().join("traffic.jsonl");
    let store = TrafficStore::default();
    store.record(record(21_000, 100));
    store.save(&path)?;
    fs::OpenOptions::new()
        .append(true)
        .open(&path)?
        .write_all(&[0xff])?;

    let restored = TrafficStore::load_at(&path, 21_000);

    assert_eq!(restored.snapshot_at(21_000).recent_requests.len(), 1);
    Ok(())
}

#[test]
fn persisted_route_counts_round_trip() -> Result<(), Box<dyn Error>> {
    let root = tempdir()?;
    let path = root.path().join("traffic.jsonl");
    let store = TrafficStore::default();
    for route in [
        TrafficRoute::HybridWs,
        TrafficRoute::HybridColdStartHttp,
        TrafficRoute::HybridRecoveryHttp,
        TrafficRoute::HybridLargeRequestHttp,
        TrafficRoute::DirectHttp,
    ] {
        let mut outcome = record(21_000, 0);
        outcome.route = Some(route);
        store.record(outcome);
    }
    store.save(&path)?;

    let restored = TrafficStore::load_at(&path, 21_000);

    assert_eq!(
        restored.route_counts(),
        TrafficRouteCounts {
            hybrid_ws: 1,
            hybrid_cold_start_http: 1,
            hybrid_recovery_http: 1,
            hybrid_large_request_http: 1,
            direct_http: 1,
        }
    );
    Ok(())
}

#[test]
fn persisted_route_counts_accept_legacy_shape() -> Result<(), Box<dyn Error>> {
    let counts: TrafficRouteCounts = serde_json::from_value(serde_json::json!({
        "hybridWs": 1,
        "hybridColdStartHttp": 2,
        "hybridRecoveryHttp": 3,
        "directHttp": 4
    }))?;

    assert_eq!(counts.hybrid_large_request_http, 0);
    Ok(())
}

#[test]
fn persisted_traffic_keeps_websocket_failure_context() -> Result<(), Box<dyn Error>> {
    let root = tempdir()?;
    let path = root.path().join("traffic.jsonl");
    let delta = serde_json::json!({
        "latestTimestampMs": 21_000,
        "nextId": 1,
        "recentRequests": [{
            "id": 1,
            "timestampMs": 21_000,
            "status": 1012,
            "path": "/v1/responses",
            "rawBytes": 0,
            "sentBytes": 0,
            "transport": "WS",
            "result": "error",
            "failurePhase": "hybridIdle",
            "failureReason": "restart"
        }],
        "buckets": []
    });
    fs::write(&path, format!("{delta}\n"))?;

    let restored = TrafficStore::load_at(&path, 21_000);
    let snapshot = restored.snapshot_at(21_000);
    let event = serde_json::to_value(
        snapshot
            .recent_requests
            .first()
            .ok_or("restored websocket failure missing")?,
    )?;

    assert_eq!(
        event.get("failurePhase"),
        Some(&serde_json::json!("hybridIdle"))
    );
    assert_eq!(
        event.get("failureReason"),
        Some(&serde_json::json!("restart"))
    );
    Ok(())
}

#[test]
fn hourly_compaction_discards_data_older_than_twenty_five_hours() -> Result<(), Box<dyn Error>> {
    let root = tempdir()?;
    let path = root.path().join("traffic.jsonl");
    let store = TrafficStore::default();
    store.record(record(10_000, 100));
    store.save(&path)?;
    store.compact_at(&path, 10_000 + RETENTION_MS + BASE_BUCKET_MS)?;

    let restored = TrafficStore::load_at(&path, 10_000 + RETENTION_MS + BASE_BUCKET_MS);

    assert!(lock(&restored.state).buckets.is_empty());
    assert!(
        restored
            .snapshot_at(10_000 + RETENTION_MS + BASE_BUCKET_MS)
            .recent_requests
            .is_empty()
    );
    assert_eq!(
        fs::read_to_string(path)?
            .lines()
            .filter(|line| !line.is_empty())
            .count(),
        1
    );
    Ok(())
}
