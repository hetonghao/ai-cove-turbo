use std::sync::Arc;

use tokio::time::{Instant, sleep, sleep_until};

use super::{HybridPool, HybridScope, PONG_TIMEOUT, total_connections};

impl HybridPool {
    pub(super) fn schedule_dormant_expiration(&self, scope: HybridScope, deadline: Instant) {
        let inner = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            sleep_until(deadline).await;
            let Some(inner) = inner.upgrade() else {
                return;
            };
            Self { inner }.expire_dormant(&scope, deadline).await;
        });
    }

    pub(super) async fn expire_dormant(&self, scope: &HybridScope, deadline: Instant) {
        loop {
            let (to_close, retry) = {
                let mut state = self.inner.state.lock().await;
                if state.dormant.get(scope) != Some(&deadline) {
                    return;
                }
                let Some(entry) = state.scopes.get_mut(scope) else {
                    state.dormant.remove(scope);
                    return;
                };
                if entry.active_local > 0 {
                    state.dormant.remove(scope);
                    return;
                }
                let result = if total_connections(entry) == entry.idle.len() {
                    let to_close = std::mem::take(&mut entry.idle);
                    state.scopes.remove(scope);
                    state.dormant.remove(scope);
                    (to_close, false)
                } else {
                    (Vec::new(), true)
                };
                drop(state);
                result
            };
            self.close_all(to_close).await;
            if !retry {
                return;
            }
            sleep(PONG_TIMEOUT).await;
        }
    }
}
