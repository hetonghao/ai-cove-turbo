use crate::proxy::private_websocket::PrivateConnectFailure;

#[derive(Debug, Default)]
pub(super) struct ScopeDiagnostics {
    pub(super) failed_attempts: u64,
    pub(super) last_failure: Option<PrivateConnectFailure>,
}

impl ScopeDiagnostics {
    pub(super) const fn record_failure(&mut self, failure: PrivateConnectFailure) {
        self.failed_attempts = self.failed_attempts.saturating_add(1);
        self.last_failure = Some(failure);
    }
}
