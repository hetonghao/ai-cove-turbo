use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use super::super::super::private_websocket::PrivateUpstream;
use super::super::cleanup::spawn_release_cleanup;
use super::super::{HybridPool, HybridScope};

pub(super) struct CheckoutLeaseGuard {
    pool: HybridPool,
    scope: HybridScope,
    session_id: u64,
    owner_active: Arc<AtomicBool>,
    upstream: Option<PrivateUpstream>,
    waiting: bool,
}

impl CheckoutLeaseGuard {
    pub(super) const fn new(
        pool: HybridPool,
        scope: HybridScope,
        session_id: u64,
        owner_active: Arc<AtomicBool>,
    ) -> Self {
        Self {
            pool,
            scope,
            session_id,
            owner_active,
            upstream: None,
            waiting: false,
        }
    }

    pub(super) const fn set_waiting(&mut self, waiting: bool) {
        self.waiting = waiting;
    }

    pub(super) fn set_upstream(&mut self, upstream: PrivateUpstream) {
        self.upstream = Some(upstream);
    }

    pub(super) const fn take_upstream(&mut self) -> Option<PrivateUpstream> {
        self.upstream.take()
    }

    pub(super) fn finish(mut self) -> Option<PrivateUpstream> {
        self.owner_active.store(false, Ordering::Release);
        self.upstream.take()
    }
}

impl Drop for CheckoutLeaseGuard {
    fn drop(&mut self) {
        self.owner_active.store(false, Ordering::Release);
        if self.waiting {
            let pool = self.pool.clone();
            let scope = self.scope.clone();
            let session_id = self.session_id;
            super::super::cleanup::spawn_cleanup("turbo-hybrid-demand-cleanup", async move {
                pool.finish_request(&scope, session_id).await;
            });
        }
        let Some(upstream) = self.upstream.take() else {
            return;
        };
        spawn_release_cleanup(
            self.pool.clone(),
            self.scope.clone(),
            self.session_id,
            upstream,
        );
    }
}
