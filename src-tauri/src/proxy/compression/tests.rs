use std::{sync::mpsc, task::Poll};

use bytes::Bytes;
use futures_util::future::poll_fn;
use tokio::sync::oneshot;

use super::CompressionScheduler;

#[tokio::test]
async fn http_encoding_keeps_threshold_and_identity_fallback() {
    let scheduler = CompressionScheduler::with_capacity(1);
    assert_eq!(
        scheduler.encode_http(Bytes::from(vec![0; 1023])).await,
        Ok(None)
    );
    let mut incompressible = vec![0_u8; 4096];
    let mut state = 0x1234_5678_u32;
    for byte in &mut incompressible {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        *byte = state.to_be_bytes()[0];
    }
    assert_eq!(
        scheduler.encode_http(Bytes::from(incompressible)).await,
        Ok(None)
    );
    let snapshot = scheduler.metrics_snapshot();
    assert_eq!(snapshot.encode_count, 2);
    assert_eq!(snapshot.fast_path_count, 1);
}

#[tokio::test]
async fn private_decode_failures_are_counted() {
    let scheduler = CompressionScheduler::with_capacity(1);
    assert!(
        scheduler
            .decode_private(Bytes::from_static(b"bad"))
            .await
            .is_err()
    );
    let snapshot = scheduler.metrics_snapshot();
    assert_eq!(snapshot.decode_count, 1);
    assert_eq!(snapshot.failures, 1);
}

#[tokio::test]
async fn private_fast_path_failures_are_counted() {
    let scheduler = CompressionScheduler::with_capacity(1);
    assert!(scheduler.encode_private(vec![0xff], false).await.is_err());
    let snapshot = scheduler.metrics_snapshot();
    assert_eq!(snapshot.encode_count, 1);
    assert_eq!(snapshot.fast_path_count, 1);
    assert_eq!(snapshot.failures, 1);
}

#[tokio::test]
async fn private_encode_and_decode_share_one_permit() {
    let scheduler = CompressionScheduler::with_capacity(1);
    let payload = vec![b'x'; 2048];
    let encoded = scheduler
        .encode_private(payload.clone(), false)
        .await
        .expect("private encoding should succeed");
    assert_eq!(scheduler.available_permits(), 1);
    let decoded = scheduler
        .decode_private(Bytes::from(encoded.bytes))
        .await
        .expect("private decoding should succeed");
    assert_eq!(decoded.payload, payload);
    assert_eq!(scheduler.available_permits(), 1);
}

#[tokio::test]
async fn concurrent_private_encode_blocks_private_decode_on_one_permit() {
    let scheduler = CompressionScheduler::with_capacity(1);
    let envelope = crate::proxy::private_websocket::encode_private_message(b"ready", false)
        .expect("fixture envelope should encode");
    let mut encode = Box::pin(scheduler.encode_private(vec![b'x'; 16 * 1024 * 1024], false));
    assert!(
        poll_fn(|context| match encode.as_mut().poll(context) {
            Poll::Pending => Poll::Ready(true),
            Poll::Ready(_) => Poll::Ready(false),
        })
        .await
    );
    let mut decode = Box::pin(scheduler.decode_private(Bytes::from(envelope)));
    assert!(
        poll_fn(|context| match decode.as_mut().poll(context) {
            Poll::Pending => Poll::Ready(true),
            Poll::Ready(_) => Poll::Ready(false),
        })
        .await
    );
    let _ = encode.await;
    let _ = decode.await;
}

#[tokio::test]
async fn concurrent_private_decode_waits_for_the_single_permit() {
    let scheduler = CompressionScheduler::with_capacity(1);
    let payload = vec![b'x'; 2048];
    let encoded = scheduler
        .encode_private(payload, false)
        .await
        .expect("private encoding should succeed");
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let first = scheduler.run(move || {
        let _ = started_tx.send(());
        let _ = release_rx.recv();
    });
    tokio::pin!(first);
    tokio::select! {
        result = &mut first => assert_eq!(result, Ok(()), "blocking fixture must wait for release"),
        _ = started_rx => {}
    }
    let mut second = Box::pin(scheduler.decode_private(Bytes::from(encoded.bytes)));
    assert!(
        poll_fn(|context| match second.as_mut().poll(context) {
            Poll::Pending => Poll::Ready(true),
            Poll::Ready(_) => Poll::Ready(false),
        })
        .await
    );
    drop(second);
    let _ = release_tx.send(());
    assert_eq!(first.await, Ok(()));
}

#[tokio::test]
async fn cancelled_waiter_releases_its_scheduler_budget() {
    let scheduler = CompressionScheduler::with_capacity(1);
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let first = scheduler.run(move || {
        let _ = started_tx.send(());
        let _ = release_rx.recv();
        1_u8
    });
    tokio::pin!(first);
    tokio::select! {
        value = &mut first => assert_eq!(value, Ok(1)),
        _ = started_rx => {}
    }
    let mut waiter = Box::pin(scheduler.run(|| 2_u8));
    assert!(
        poll_fn(|context| match waiter.as_mut().poll(context) {
            Poll::Pending => Poll::Ready(true),
            Poll::Ready(_) => Poll::Ready(false),
        })
        .await
    );
    drop(waiter);
    let _ = release_tx.send(());
    assert_eq!(first.await, Ok(1));
    tokio::task::yield_now().await;
    assert_eq!(scheduler.available_permits(), 1);
}

#[tokio::test]
async fn aborted_running_work_keeps_permit_until_worker_exit() {
    let scheduler = CompressionScheduler::with_capacity(1);
    let (started_tx, started_rx) = oneshot::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let task = tokio::spawn({
        let scheduler = scheduler.clone();
        async move {
            scheduler
                .run(move || {
                    let _ = started_tx.send(());
                    let _ = release_rx.recv();
                })
                .await
        }
    });
    let _ = started_rx.await;
    task.abort();
    assert_eq!(scheduler.available_permits(), 0);
    let _ = release_tx.send(());
    let _ = task.await;
    tokio::task::yield_now().await;
    assert_eq!(scheduler.available_permits(), 1);
}
