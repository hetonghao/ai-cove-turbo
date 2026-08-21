use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use super::super::private_websocket::PrivateUpstream;
use super::cleanup::spawn_release_cleanup;
use super::{HybridPool, HybridScope, LeaseRetirement};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::proxy) enum LeaseState {
    Active,
    Released,
    Discarded,
    Parked,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::proxy) enum LeaseParkError {
    Unavailable,
}

pub(in crate::proxy) struct Lease {
    pool: HybridPool,
    scope: HybridScope,
    session_id: u64,
    owner_active: Arc<AtomicBool>,
    upstream: Option<PrivateUpstream>,
    state: LeaseState,
}

impl Lease {
    pub(in crate::proxy) const fn active(
        pool: HybridPool,
        scope: HybridScope,
        session_id: u64,
        owner_active: Arc<AtomicBool>,
        upstream: PrivateUpstream,
    ) -> Self {
        Self {
            pool,
            scope,
            session_id,
            owner_active,
            upstream: Some(upstream),
            state: LeaseState::Active,
        }
    }

    #[cfg(test)]
    pub(in crate::proxy) const fn state(&self) -> LeaseState {
        self.state
    }

    pub(in crate::proxy) const fn upstream_mut(&mut self) -> Option<&mut PrivateUpstream> {
        self.upstream.as_mut()
    }

    pub(in crate::proxy) const fn take_upstream(&mut self) -> Option<PrivateUpstream> {
        self.upstream.take()
    }

    pub(in crate::proxy) fn put_upstream(&mut self, upstream: PrivateUpstream) {
        self.upstream = Some(upstream);
    }

    #[cfg(test)]
    pub(in crate::proxy) const fn active_without_upstream(
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
            state: LeaseState::Active,
        }
    }

    pub(in crate::proxy) async fn release(&mut self) {
        if self.state != LeaseState::Active {
            return;
        }
        let upstream = self.upstream.take();
        self.pool
            .release_session_connection(&self.scope, self.session_id, upstream)
            .await;
        self.finish(LeaseState::Released);
    }

    pub(in crate::proxy) async fn discard(&mut self, retirement: LeaseRetirement) {
        if self.state != LeaseState::Active {
            return;
        }
        let upstream = self.upstream.take();
        self.pool
            .discard(&self.scope, self.session_id, retirement)
            .await;
        if let Some(upstream) = upstream {
            self.pool.close_all(vec![upstream]).await;
        }
        self.finish(LeaseState::Discarded);
    }

    pub(in crate::proxy) async fn park(
        &mut self,
        thread_id: String,
        response_id: String,
    ) -> Result<(), LeaseParkError> {
        if self.state != LeaseState::Active {
            return Ok(());
        }
        if thread_id.is_empty() || response_id.is_empty() {
            return Err(LeaseParkError::Unavailable);
        }
        let Some(upstream) = self.upstream.take() else {
            return Err(LeaseParkError::Unavailable);
        };
        match self
            .pool
            .park_session_connection(
                &self.scope,
                self.session_id,
                thread_id,
                response_id,
                upstream,
            )
            .await
        {
            Ok(()) => {
                self.finish(LeaseState::Parked);
                Ok(())
            }
            Err(upstream) => {
                self.upstream = Some(upstream);
                Err(LeaseParkError::Unavailable)
            }
        }
    }

    fn finish(&mut self, state: LeaseState) {
        self.state = state;
        self.owner_active.store(false, Ordering::Release);
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        if self.state != LeaseState::Active {
            return;
        }
        self.owner_active.store(false, Ordering::Release);
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
