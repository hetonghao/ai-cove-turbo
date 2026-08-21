use std::{future::Future, io::Write};

use super::super::private_websocket::PrivateUpstream;
use super::{HybridPool, HybridScope};

pub(super) fn spawn_cleanup<F>(task_name: &'static str, cleanup: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(cleanup);
        return;
    }

    match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime.block_on(cleanup),
        Err(error) => {
            let mut stderr = std::io::stderr().lock();
            drop(writeln!(
                stderr,
                "{task_name} runtime fallback failed: {error}"
            ));
        }
    }
}

pub(super) fn spawn_release_cleanup(
    pool: HybridPool,
    scope: HybridScope,
    session_id: u64,
    upstream: PrivateUpstream,
) {
    spawn_cleanup("turbo-hybrid-release-cleanup", async move {
        pool.release_session_connection(&scope, session_id, Some(upstream))
            .await;
    });
}
