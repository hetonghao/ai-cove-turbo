use std::{collections::HashMap, io, sync::Arc, time::Duration};

use axum::Router;
use tokio::{
    net::TcpListener,
    sync::{Mutex, Notify, oneshot},
    task::JoinHandle,
};
use url::Url;

#[derive(Clone, Copy)]
pub(super) enum PrivateBehavior {
    Delay,
    Fail,
    FailFirstBatch,
    HoldResponse,
    HoldResponseNoPong,
    ProbeDelay,
    HttpPayloadTooLarge,
    IdleError,
    IdleMessage,
    IdleRestart,
    IdleRestartDelayedReconnect,
    IdleUnexpectedEof,
    ActiveFailure,
    ActiveTransportFallback,
    ActiveGenericFallbackLookalike,
    ActiveOutputThenTransportFallback,
    ActiveCancelThenTransportFallback,
    ActiveTransportFallbackHttpFailure,
    ActiveMessageTooBig,
    ActiveReplayRequired,
    Stateful,
    CancelledTerminal,
    Persistent,
    TerminalTail,
}

impl PrivateBehavior {
    pub(super) const fn holds_response(self) -> bool {
        matches!(self, Self::HoldResponse | Self::HoldResponseNoPong)
    }

    pub(super) const fn keeps_connection_open(self) -> bool {
        matches!(
            self,
            Self::HoldResponse
                | Self::HoldResponseNoPong
                | Self::ProbeDelay
                | Self::HttpPayloadTooLarge
                | Self::IdleError
                | Self::IdleMessage
                | Self::ActiveTransportFallback
                | Self::ActiveGenericFallbackLookalike
                | Self::ActiveOutputThenTransportFallback
                | Self::ActiveCancelThenTransportFallback
                | Self::ActiveTransportFallbackHttpFailure
                | Self::ActiveMessageTooBig
                | Self::Stateful
                | Self::Persistent
                | Self::CancelledTerminal
                | Self::TerminalTail
        )
    }

    pub(super) const fn idle_close(self) -> Option<(u16, &'static str)> {
        match self {
            Self::IdleRestart | Self::IdleRestartDelayedReconnect => Some((1012, "restart")),
            Self::IdleUnexpectedEof => Some((1011, "unexpected EOF")),
            Self::Delay
            | Self::Fail
            | Self::FailFirstBatch
            | Self::HoldResponse
            | Self::HoldResponseNoPong
            | Self::ProbeDelay
            | Self::HttpPayloadTooLarge
            | Self::IdleError
            | Self::IdleMessage
            | Self::ActiveFailure
            | Self::ActiveTransportFallback
            | Self::ActiveGenericFallbackLookalike
            | Self::ActiveOutputThenTransportFallback
            | Self::ActiveCancelThenTransportFallback
            | Self::ActiveTransportFallbackHttpFailure
            | Self::ActiveMessageTooBig
            | Self::ActiveReplayRequired
            | Self::Stateful
            | Self::CancelledTerminal
            | Self::Persistent
            | Self::TerminalTail => None,
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct FixtureConfig {
    pub(super) private: PrivateBehavior,
    pub(super) delay_http: bool,
}

#[derive(Default)]
pub(super) struct Counts {
    pub(super) private_handshakes: usize,
    pub(super) private_ready: usize,
    pub(super) private_messages: usize,
    pub(super) active_ready: usize,
    pub(super) active_pings: usize,
    pub(super) private_normal_closes: usize,
    pub(super) http_requests: usize,
    pub(super) close_frames_sent: usize,
    pub(super) private_ready_by_scope: HashMap<String, usize>,
}

pub(super) struct FixtureState {
    pub(super) counts: Mutex<Counts>,
    pub(super) changed: Notify,
    pub(super) release_private: Notify,
    pub(super) release_http: Notify,
}

#[derive(Clone)]
pub(super) struct Fixture {
    pub(super) upstream: Url,
    pub(super) config: FixtureConfig,
    pub(super) state: Arc<FixtureState>,
}

pub(super) struct FixtureServer {
    pub(super) fixture: Fixture,
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

#[derive(Clone, Copy)]
enum CountKind {
    PrivateHandshake,
    PrivateReady,
    PrivateMessage,
    ActiveReady,
    PrivateNormalClose,
    Http,
    CloseFrame,
}

impl FixtureServer {
    pub(super) async fn start(config: FixtureConfig) -> io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let address = listener.local_addr()?;
        let upstream = Url::parse(&format!("http://{address}/v1")).map_err(io::Error::other)?;
        let fixture = Fixture {
            upstream,
            config,
            state: Arc::new(FixtureState {
                counts: Mutex::new(Counts::default()),
                changed: Notify::new(),
                release_private: Notify::new(),
                release_http: Notify::new(),
            }),
        };
        let (shutdown, receiver) = oneshot::channel();
        let app = Router::new()
            .fallback(super::integration_server::upstream_request)
            .with_state(fixture.clone());
        let task = tokio::spawn(async move {
            let server = axum::serve(listener, app).with_graceful_shutdown(async move {
                let _ = receiver.await;
            });
            let _ = server.await;
        });
        Ok(Self {
            fixture,
            shutdown: Some(shutdown),
            task,
        })
    }

    pub(super) async fn stop(mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        let _ = self.task.await;
    }
}

impl Fixture {
    pub(super) async fn wait_private(&self, expected: usize) -> io::Result<()> {
        self.wait_count(CountKind::PrivateHandshake, expected).await
    }

    pub(super) async fn wait_ready(&self, expected: usize) -> io::Result<()> {
        self.wait_count(CountKind::PrivateReady, expected).await
    }

    pub(super) async fn wait_ready_for_scope(&self, scope: &str) -> io::Result<()> {
        self.wait_ready_for_scope_count(scope, 1).await
    }

    pub(super) async fn ready_for_scope_count(&self, scope: &str) -> usize {
        self.state
            .counts
            .lock()
            .await
            .private_ready_by_scope
            .get(scope)
            .copied()
            .unwrap_or_default()
    }

    pub(super) async fn wait_ready_for_scope_count(
        &self,
        scope: &str,
        expected: usize,
    ) -> io::Result<()> {
        let scope = scope.to_owned();
        let wait = async {
            loop {
                let changed = self.state.changed.notified();
                if self.ready_for_scope_count(&scope).await >= expected {
                    return;
                }
                changed.await;
            }
        };
        tokio::time::timeout(Duration::from_secs(10), wait)
            .await
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    "scope prewarm did not become ready",
                )
            })
    }

    pub(super) async fn wait_messages(&self, expected: usize) -> io::Result<()> {
        self.wait_count(CountKind::PrivateMessage, expected).await
    }

    pub(super) async fn wait_active_ready(&self, expected: usize) -> io::Result<()> {
        self.wait_count(CountKind::ActiveReady, expected).await
    }

    pub(super) async fn wait_active_pings(&self, expected: usize) -> io::Result<()> {
        for _ in 0..1_000 {
            if self.state.counts.lock().await.active_pings >= expected {
                return Ok(());
            }
            tokio::task::yield_now().await;
        }
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "active ping did not arrive",
        ))
    }

    pub(super) async fn wait_normal_closes(&self, expected: usize) -> io::Result<()> {
        self.wait_count(CountKind::PrivateNormalClose, expected)
            .await
    }

    pub(super) async fn wait_http(&self, expected: usize) -> io::Result<()> {
        self.wait_count(CountKind::Http, expected).await
    }

    pub(super) async fn wait_close_frames(&self, expected: usize) -> io::Result<()> {
        self.wait_count(CountKind::CloseFrame, expected).await
    }

    pub(super) async fn counts(&self) -> CountsSnapshot {
        let counts = self.state.counts.lock().await;
        CountsSnapshot {
            private_handshakes: counts.private_handshakes,
            private_messages: counts.private_messages,
            active_pings: counts.active_pings,
            http_requests: counts.http_requests,
        }
    }

    pub(super) fn release_private(&self) {
        self.state.release_private.notify_one();
    }

    pub(super) fn release_private_all(&self) {
        self.state.release_private.notify_waiters();
    }

    pub(super) fn release_http(&self) {
        self.state.release_http.notify_one();
    }

    pub(super) async fn record(&self, update: impl FnOnce(&mut Counts)) {
        let mut counts = self.state.counts.lock().await;
        update(&mut counts);
        self.state.changed.notify_waiters();
    }

    async fn wait_count(&self, kind: CountKind, expected: usize) -> io::Result<()> {
        self.wait_count_with_timeout(kind, expected, Duration::from_secs(3))
            .await
    }

    async fn wait_count_with_timeout(
        &self,
        kind: CountKind,
        expected: usize,
        timeout: Duration,
    ) -> io::Result<()> {
        let wait = async {
            loop {
                let changed = self.state.changed.notified();
                let current = {
                    let counts = self.state.counts.lock().await;
                    match kind {
                        CountKind::PrivateHandshake => counts.private_handshakes,
                        CountKind::PrivateReady => counts.private_ready,
                        CountKind::PrivateMessage => counts.private_messages,
                        CountKind::ActiveReady => counts.active_ready,
                        CountKind::PrivateNormalClose => counts.private_normal_closes,
                        CountKind::Http => counts.http_requests,
                        CountKind::CloseFrame => counts.close_frames_sent,
                    }
                };
                if current >= expected {
                    return;
                }
                changed.await;
            }
        };
        tokio::time::timeout(timeout, wait)
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "fixture event timed out"))
    }
}

#[derive(Clone, Copy)]
pub(super) struct CountsSnapshot {
    pub(super) private_handshakes: usize,
    pub(super) private_messages: usize,
    pub(super) active_pings: usize,
    pub(super) http_requests: usize,
}
