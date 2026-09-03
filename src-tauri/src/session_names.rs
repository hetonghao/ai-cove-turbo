use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use tokio::{
    sync::{Mutex, Notify, oneshot},
    time::{self, Instant},
};

use tauri::async_runtime::JoinHandle;

use crate::{
    codex_thread_title::{CodexThreadInfo, ThreadInfoReadError, is_codex_thread_id, read_batch},
    proxy::{ConnectionSnapshot, traffic::RequestEvent},
};

#[path = "session_name_state.rs"]
mod state;

#[cfg(test)]
use state::{SESSION_NAME_BATCH_MIN_INTERVAL, SESSION_NAME_MAX_BATCH};
use state::{SessionNameEntry, SessionNameState, has_name};

pub(crate) const SESSION_NAME_REFRESH_INTERVAL: Duration = Duration::from_secs(20 * 60);
const SESSION_NAME_TICK: Duration = Duration::from_secs(60);
const SESSION_NAME_BATCH_TIMEOUT: Duration = Duration::from_secs(3);

pub(crate) type SessionNameSnapshot = HashMap<String, Option<CodexThreadInfo>>;

#[derive(Debug)]
pub(crate) struct SessionNameCache {
    database: PathBuf,
    state: Mutex<SessionNameState>,
    wake: Notify,
}

#[derive(Debug)]
pub(crate) struct SessionNameTask {
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl SessionNameTask {
    pub(crate) async fn stop(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        let _ = self.task.await;
    }
}

impl SessionNameCache {
    pub(crate) fn new(database: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            database,
            state: Mutex::new(SessionNameState::default()),
            wake: Notify::new(),
        })
    }

    pub(crate) fn start(self: &Arc<Self>) -> SessionNameTask {
        let (stop, stopped) = oneshot::channel();
        let cache = Arc::clone(self);
        let task = tauri::async_runtime::spawn(async move {
            cache.run(stopped).await;
        });
        SessionNameTask {
            stop: Some(stop),
            task,
        }
    }

    pub(crate) async fn observe_requests(&self, requests: &[RequestEvent]) {
        let request_ids = requests
            .iter()
            .filter_map(RequestEvent::thread_id)
            .filter(|thread_id| is_codex_thread_id(thread_id))
            .map(str::to_owned)
            .collect::<HashSet<_>>();
        let mut changed = false;
        let now = Instant::now();
        let mut state = self.state.lock().await;
        for thread_id in &request_ids {
            let entry = state.entries.entry(thread_id.clone()).or_insert_with(|| {
                changed = true;
                SessionNameEntry::new(now)
            });
            if entry.temporary_name.is_none()
                && let Some(name) = requests
                    .iter()
                    .filter(|request| request.thread_id() == Some(thread_id.as_str()))
                    .find_map(|request| request.temporary_name.as_deref())
            {
                entry.temporary_name = Some(name.to_owned());
                changed = true;
            }
            if !entry.request_visible {
                entry.request_visible = true;
                changed = true;
            }
        }
        for (thread_id, entry) in &mut state.entries {
            if entry.request_visible && !request_ids.contains(thread_id) {
                entry.request_visible = false;
                changed = true;
            }
        }
        changed |= state.prune();
        drop(state);
        if changed {
            self.wake.notify_one();
        }
    }

    pub(crate) async fn observe_hints(&self, hints: &HashMap<String, String>) {
        let mut changed = false;
        let now = Instant::now();
        let mut state = self.state.lock().await;
        for (thread_id, name) in hints {
            if !is_codex_thread_id(thread_id) || name.trim().is_empty() {
                continue;
            }
            let entry = state.entries.entry(thread_id.clone()).or_insert_with(|| {
                changed = true;
                SessionNameEntry::new(now)
            });
            if entry.temporary_name.is_none() {
                entry.temporary_name = Some(name.clone());
                changed = true;
            }
            if !entry.request_visible {
                entry.request_visible = true;
                changed = true;
            }
        }
        changed |= state.prune();
        drop(state);
        if changed {
            self.wake.notify_one();
        }
    }

    pub(crate) async fn observe_connections(&self, snapshot: &ConnectionSnapshot) {
        let connection_ids = snapshot
            .bound_threads
            .iter()
            .map(|item| item.thread_id.as_str())
            .chain(
                snapshot
                    .transitions
                    .iter()
                    .filter_map(|item| item.thread_id.as_deref()),
            )
            .chain(
                snapshot
                    .recent_closed
                    .iter()
                    .filter_map(|item| item.thread_id.as_deref()),
            )
            .filter(|thread_id| is_codex_thread_id(thread_id))
            .map(str::to_owned)
            .collect::<HashSet<_>>();
        let mut changed = false;
        let now = Instant::now();
        let mut state = self.state.lock().await;
        for thread_id in &connection_ids {
            let entry = state.entries.entry(thread_id.clone()).or_insert_with(|| {
                changed = true;
                SessionNameEntry::new(now)
            });
            if !entry.connection_visible {
                entry.connection_visible = true;
                entry.connection_was_visible = true;
                changed = true;
            }
        }
        for (thread_id, entry) in &mut state.entries {
            if entry.connection_visible && !connection_ids.contains(thread_id) {
                entry.connection_visible = false;
                entry.connection_was_visible = true;
                changed = true;
            }
        }
        changed |= state.prune();
        drop(state);
        if changed {
            self.wake.notify_one();
        }
    }

    pub(crate) async fn snapshot(&self) -> SessionNameSnapshot {
        let state = self.state.lock().await;
        state
            .entries
            .iter()
            .filter(|(_, entry)| entry.connection_visible || entry.request_visible)
            .map(|(thread_id, entry)| {
                let info = entry.info.clone().or_else(|| {
                    entry.temporary_name.as_ref().map(|name| CodexThreadInfo {
                        name: Some(name.clone()),
                        parent_name: None,
                        is_subagent: false,
                    })
                });
                let info = info.map(|mut info| {
                    if !has_name(&info) {
                        info.name.clone_from(&entry.temporary_name);
                    }
                    info
                });
                (thread_id.clone(), info)
            })
            .collect()
    }

    async fn run(&self, mut stop: oneshot::Receiver<()>) {
        let mut ticker = time::interval(SESSION_NAME_TICK);
        ticker.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = &mut stop => break,
                _ = ticker.tick() => self.refresh_due().await,
                () = self.wake.notified() => self.refresh_due().await,
            }
        }
    }

    async fn refresh_due(&self) {
        let now = Instant::now();
        let due = {
            let mut state = self.state.lock().await;
            state.select_due(now)
        };
        let Some(due) = due else {
            return;
        };
        let thread_ids = due
            .iter()
            .map(|(thread_id, _)| thread_id.clone())
            .collect::<Vec<_>>();

        let result = time::timeout(
            SESSION_NAME_BATCH_TIMEOUT,
            read_batch(self.database.clone(), thread_ids.clone()),
        )
        .await
        .map_or(Err(ThreadInfoReadError::Timeout), |result| result);
        let mut state = self.state.lock().await;
        let now = Instant::now();
        state.apply_result(&due, result, now);
        drop(state);
        let has_new_due = {
            let state = self.state.lock().await;
            state.has_new_due(Instant::now())
        };
        if has_new_due {
            self.wake.notify_one();
        }
    }
}

#[cfg(test)]
#[path = "session_names_tests.rs"]
mod tests;
