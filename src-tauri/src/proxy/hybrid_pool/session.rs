use std::{
    fmt,
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use super::super::private_websocket::PrivateUpstream;
use super::{HybridPool, HybridScope, Lease, LeaseRetirement};
use tokio::sync::Notify;

mod checkout;
use super::cleanup::{spawn_cleanup, spawn_release_cleanup};
use checkout::CheckoutLeaseGuard;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::proxy) enum HandoffCheckoutFailure {
    Preflight,
}

pub(in crate::proxy) struct SessionHandle {
    pool: HybridPool,
    scope: HybridScope,
    session_id: u64,
    closed: AtomicBool,
    closed_notify: Notify,
    lease_active: Arc<AtomicBool>,
}

impl SessionHandle {
    pub(in crate::proxy) fn new(pool: HybridPool, scope: HybridScope, session_id: u64) -> Self {
        Self {
            pool,
            scope,
            session_id,
            closed: AtomicBool::new(false),
            closed_notify: Notify::new(),
            lease_active: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(in crate::proxy) async fn checkout(&self) -> Option<Lease> {
        self.checkout_with(|| self.pool.checkout(&self.scope, self.session_id))
            .await
    }

    pub(in crate::proxy) async fn checkout_wait(&self, wait: Duration) -> Option<Lease> {
        self.checkout_with(|| self.pool.checkout_wait(&self.scope, self.session_id, wait))
            .await
    }

    pub(in crate::proxy) async fn checkout_with<F, Fut>(&self, checkout: F) -> Option<Lease>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Option<PrivateUpstream>>,
    {
        if self.closed.load(Ordering::Acquire)
            || self
                .lease_active
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return None;
        }
        let mut guard = CheckoutLeaseGuard::new(
            self.pool.clone(),
            self.scope.clone(),
            self.session_id,
            Arc::clone(&self.lease_active),
        );
        let Some(upstream) = checkout().await else {
            self.lease_active.store(false, Ordering::Release);
            return None;
        };
        guard.set_upstream(upstream);
        if self.closed.load(Ordering::Acquire) {
            if let Some(upstream) = guard.take_upstream() {
                spawn_release_cleanup(
                    self.pool.clone(),
                    self.scope.clone(),
                    self.session_id,
                    upstream,
                );
            }
            self.lease_active.store(false, Ordering::Release);
            return None;
        }
        let upstream = guard.finish()?;
        Some(Lease::active(
            self.pool.clone(),
            self.scope.clone(),
            self.session_id,
            Arc::clone(&self.lease_active),
            upstream,
        ))
    }

    pub(in crate::proxy) async fn checkout_handoff_wait(
        &self,
        thread_id: &str,
        response_id: &str,
    ) -> Result<Option<Lease>, HandoffCheckoutFailure> {
        if self.closed.load(Ordering::Acquire)
            || self
                .lease_active
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return Ok(None);
        }
        let mut guard = CheckoutLeaseGuard::new(
            self.pool.clone(),
            self.scope.clone(),
            self.session_id,
            Arc::clone(&self.lease_active),
        );
        let result = self
            .pool
            .checkout_handoff_wait(&self.scope, self.session_id, thread_id, response_id)
            .await;
        let upstream = match result {
            Ok(Some(upstream)) => upstream,
            Ok(None) => return Ok(None),
            Err(error) => return Err(error),
        };
        guard.set_upstream(upstream);
        if self.closed.load(Ordering::Acquire) {
            if let Some(upstream) = guard.take_upstream() {
                spawn_release_cleanup(
                    self.pool.clone(),
                    self.scope.clone(),
                    self.session_id,
                    upstream,
                );
            }
            self.lease_active.store(false, Ordering::Release);
            return Ok(None);
        }
        let Some(upstream) = guard.finish() else {
            return Ok(None);
        };
        Ok(Some(Lease::active(
            self.pool.clone(),
            self.scope.clone(),
            self.session_id,
            Arc::clone(&self.lease_active),
            upstream,
        )))
    }

    pub(in crate::proxy) async fn observe(&self, observation: super::ConnectionObservation) {
        self.pool
            .observe_session(self.session_id, observation)
            .await;
    }

    pub(in crate::proxy) async fn record_response_create(&self) {
        self.pool
            .record_response_create(&self.scope, self.session_id)
            .await;
    }

    pub(in crate::proxy) async fn has_initialized(&self) -> bool {
        self.pool.has_initialized(&self.scope).await
    }

    pub(in crate::proxy) async fn checkout_ready(&self) -> Option<Lease> {
        loop {
            if self.is_closed() {
                return None;
            }
            if let Some(lease) = self.checkout().await {
                return Some(lease);
            }
            tokio::select! {
                () = self.pool.inner.ready.notified() => {}
                () = self.closed_notify.notified() => return None,
            }
        }
    }

    pub(in crate::proxy) async fn release_unleased(&self) {
        self.pool
            .release_session_connection(&self.scope, self.session_id, None)
            .await;
    }

    pub(in crate::proxy) async fn discard_unleased(&self, retirement: LeaseRetirement) {
        self.pool
            .discard(&self.scope, self.session_id, retirement)
            .await;
    }

    pub(in crate::proxy) async fn close(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        self.closed_notify.notify_one();
        self.pool.unregister(&self.scope, self.session_id).await;
    }

    pub(in crate::proxy) fn detach_after_park(&self) {
        self.closed.store(true, Ordering::Release);
        self.closed_notify.notify_one();
    }

    pub(in crate::proxy) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(in crate::proxy) fn is_lease_active(&self) -> bool {
        self.lease_active.load(Ordering::Acquire)
    }
}

impl fmt::Debug for SessionHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionHandle")
            .field("session_id", &self.session_id)
            .field("closed", &self.is_closed())
            .finish_non_exhaustive()
    }
}

impl Drop for SessionHandle {
    fn drop(&mut self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        self.closed_notify.notify_one();
        let pool = self.pool.clone();
        let scope = self.scope.clone();
        let session_id = self.session_id;
        let cleanup = async move { pool.unregister(&scope, session_id).await };
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(cleanup);
            return;
        }
        spawn_cleanup("turbo-hybrid-session-cleanup", cleanup);
    }
}
