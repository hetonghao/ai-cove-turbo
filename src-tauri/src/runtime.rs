#![allow(clippy::assigning_clones)]

use std::{
    collections::{BTreeMap, HashSet},
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard, OnceLock, RwLock, RwLockReadGuard, RwLockWriteGuard, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[cfg(all(not(test), target_os = "macos"))]
use std::process::Command;

use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use tokio::sync::{Mutex as AsyncMutex, oneshot};
use url::Url;

use crate::{
    catalog::{self, CatalogMetadata, CatalogModel, CatalogModelUpdate, CatalogStatus},
    catalog_discovery::{self, DiscoveryResult},
    codex_thread_title::CodexThreadInfo,
    config::{
        AI_COVE_UPSTREAM, ConfigError, ManagedConfig, ManagedOwnership, Preflight, RestoreOutcome,
        SessionHandoff, StaleRecovery, UpstreamCompatibility, managed_ownership, preflight,
        read_session_handoff, recover_stale, relinquish_websocket, remove_session_handoff, restore,
        set_ai_cove_upstream as replace_loopback_upstream, set_managed_websocket, take_over,
        upstream_compatibility, validate_upstream_override, write_session_handoff,
    },
    proxy::{
        CapabilityModelStatus, ConnectionSnapshot, Metrics, ModelPolicyStatus, ModelPolicyUpdate,
        ProxyHandle, ProxyOptions, effective_auth_headers, start_proxy_with_policy,
        traffic::{RequestEvent, TrafficWindow},
    },
    session_names::{SessionNameCache, SessionNameSnapshot, SessionNameTask},
};

const DEFAULT_PORT: u16 = 44_175;
const TRAFFIC_SAVE_INTERVAL: Duration = Duration::from_secs(30);
const TRAFFIC_COMPACT_INTERVAL: Duration = Duration::from_secs(60 * 60);
const CATALOG_SYNC_MIN_INTERVAL_MS: u64 = 5_000;

fn codex_database_path(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
        .join("state_5.sqlite")
}

#[derive(Clone, Debug)]
pub(crate) struct RuntimePaths {
    pub(crate) config_path: PathBuf,
    pub(crate) data_dir: PathBuf,
}

impl RuntimePaths {
    fn recovery_path(&self) -> PathBuf {
        self.data_dir.join("recovery.json")
    }

    fn session_handoff_path(&self) -> PathBuf {
        self.data_dir.join("handoff.json")
    }

    fn catalog_recovery_path(&self) -> PathBuf {
        self.data_dir.join("catalog-recovery.json")
    }

    fn preferences_path(&self) -> PathBuf {
        self.data_dir.join("preferences.json")
    }

    fn model_policy_path(&self) -> PathBuf {
        self.data_dir.join("ai_cove_turbo_model_policy.json")
    }

    fn model_settings_journal_path(&self) -> PathBuf {
        self.data_dir
            .join("ai_cove_turbo_model_settings.journal.json")
    }

    fn traffic_path(&self) -> PathBuf {
        self.data_dir.join("traffic.jsonl")
    }
}

// CLIPPY-ALLOW: 各偏好项是独立持久化开关，合并会破坏配置字段语义。
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
struct Preferences {
    compression_enabled: bool,
    websocket_enabled: bool,
    autostart_initialized: bool,
    dock_visible: bool,
    dock_initialized: bool,
    last_port: Option<u16>,
    confirmed_non_ai_cove_upstream: Option<String>,
    upstream_override: Option<String>,
}

const MODEL_SETTINGS_JOURNAL_VERSION: u8 = 1;

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ModelSettingsJournal {
    version: u8,
    catalog_before: Vec<u8>,
    catalog_before_revision: String,
    catalog_after_revision: Option<String>,
    metadata_before: CatalogMetadata,
    #[serde(default)]
    metadata_after: Option<CatalogMetadata>,
    policy_before: Option<Vec<u8>>,
    policy_after_revision: Option<String>,
    #[serde(default)]
    policy_after: Option<JournalPolicy>,
    committed: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct JournalPolicy {
    default_transport: String,
    models: BTreeMap<String, String>,
}

struct ModelSettingsJournalContext {
    before: Vec<u8>,
    metadata_before: CatalogMetadata,
    policy_path: PathBuf,
    journal_path: PathBuf,
    journal: ModelSettingsJournal,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            compression_enabled: true,
            websocket_enabled: true,
            autostart_initialized: false,
            dock_visible: true,
            dock_initialized: false,
            last_port: None,
            confirmed_non_ai_cove_upstream: None,
            upstream_override: None,
        }
    }
}

// CLIPPY-ALLOW: 状态字段直接对应前端契约，拆分会增加跨层映射。
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AppStatus {
    pub(crate) service_healthy: bool,
    pub(crate) endpoint: String,
    pub(crate) config_state: String,
    pub(crate) config_message: String,
    pub(crate) provider: String,
    pub(crate) upstream: String,
    pub(crate) original_upstream: String,
    pub(crate) ai_cove_upstream: bool,
    pub(crate) ai_cove_upstream_fix_available: bool,
    pub(crate) compression_enabled: bool,
    pub(crate) compression_verified: bool,
    pub(crate) websocket_enabled: bool,
    pub(crate) websocket_verified: bool,
    pub(crate) websocket_zstd_verified: bool,
    pub(crate) websocket_state: String,
    pub(crate) prewarm_state: String,
    pub(crate) model_policy: ModelPolicyStatus,
    pub(crate) transport_capabilities: std::collections::HashMap<String, CapabilityModelStatus>,
    pub(crate) transport_capability_reason: Option<String>,
    pub(crate) websocket_handshakes: u64,
    pub(crate) websocket_raw_bytes: u64,
    pub(crate) websocket_sent_bytes: u64,
    pub(crate) websocket_messages: u64,
    pub(crate) http_fallbacks: u64,
    pub(crate) hybrid_ws: u64,
    pub(crate) hybrid_cold_start_http: u64,
    pub(crate) hybrid_recovery_http: u64,
    pub(crate) hybrid_policy_http: u64,
    pub(crate) hybrid_capability_http: u64,
    pub(crate) hybrid_large_request_http: u64,
    pub(crate) direct_http: u64,
    pub(crate) recent_requests: Vec<RequestEvent>,
    pub(crate) session_names: SessionNameSnapshot,
    pub(crate) traffic_windows: Vec<TrafficWindow>,
    pub(crate) autostart_enabled: bool,
    pub(crate) dock_visible: bool,
    pub(crate) dock_control_available: bool,
    pub(crate) codex_state: String,
    pub(crate) restart_required: bool,
    pub(crate) desktop_restarted: bool,
    pub(crate) requests: u64,
    pub(crate) raw_bytes: u64,
    pub(crate) sent_bytes: u64,
    pub(crate) compression_ratio: f64,
    pub(crate) update_state: String,
    pub(crate) update_message: String,
    pub(crate) update_progress: u8,
    pub(crate) catalog: CatalogStatus,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModelSettingsSaveStatus {
    pub(crate) catalog: CatalogStatus,
    pub(crate) model_policy: ModelPolicyStatus,
    pub(crate) rolled_back: bool,
    pub(crate) partial_failure: bool,
    pub(crate) error: Option<String>,
}

impl AppStatus {
    fn starting(preferences: &Preferences) -> Self {
        Self {
            service_healthy: false,
            endpoint: "—".to_owned(),
            config_state: "starting".to_owned(),
            config_message: "正在检查 Codex 配置".to_owned(),
            provider: "—".to_owned(),
            upstream: "—".to_owned(),
            original_upstream: "—".to_owned(),
            ai_cove_upstream: false,
            ai_cove_upstream_fix_available: false,
            compression_enabled: preferences.compression_enabled,
            compression_verified: false,
            websocket_enabled: preferences.websocket_enabled,
            websocket_verified: false,
            websocket_zstd_verified: false,
            websocket_state: if preferences.websocket_enabled {
                "waiting".to_owned()
            } else {
                "disabled".to_owned()
            },
            prewarm_state: "disabled".to_owned(),
            model_policy: ModelPolicyStatus {
                default_transport: "auto".to_owned(),
                models: std::collections::HashMap::new(),
                reason: None,
            },
            transport_capabilities: std::collections::HashMap::new(),
            transport_capability_reason: None,
            websocket_handshakes: 0,
            websocket_raw_bytes: 0,
            websocket_sent_bytes: 0,
            websocket_messages: 0,
            http_fallbacks: 0,
            hybrid_ws: 0,
            hybrid_cold_start_http: 0,
            hybrid_recovery_http: 0,
            hybrid_policy_http: 0,
            hybrid_capability_http: 0,
            hybrid_large_request_http: 0,
            direct_http: 0,
            recent_requests: Vec::new(),
            session_names: std::collections::HashMap::<String, Option<CodexThreadInfo>>::new(),
            traffic_windows: Vec::new(),
            autostart_enabled: true,
            dock_visible: preferences.dock_visible,
            dock_control_available: cfg!(target_os = "macos"),
            codex_state: "checking".to_owned(),
            restart_required: false,
            desktop_restarted: false,
            requests: 0,
            raw_bytes: 0,
            sent_bytes: 0,
            compression_ratio: 0.0,
            update_state: "idle".to_owned(),
            update_message: "尚未检查更新".to_owned(),
            update_progress: 0,
            catalog: CatalogStatus {
                path: "—".to_owned(),
                state: "starting".to_owned(),
                source_path: None,
                models: Vec::new(),
                changes: Vec::new(),
                restart_required: false,
                loaded: false,
                request_verified: false,
                revision: String::new(),
                metadata: CatalogMetadata::default(),
            },
        }
    }
}

#[derive(Debug)]
pub(crate) struct AppRuntime {
    paths: RuntimePaths,
    preferences: Mutex<Preferences>,
    status: RwLock<AppStatus>,
    catalog: Mutex<CatalogStatus>,
    catalog_write_lock: AsyncMutex<()>,
    catalog_last_sync_ms: AtomicU64,
    catalog_sync_running: AtomicBool,
    compression_enabled: Arc<AtomicBool>,
    websocket_enabled: Arc<AtomicBool>,
    metrics: Arc<Metrics>,
    managed: Mutex<Option<ManagedConfig>>,
    proxy: AsyncMutex<Option<ProxyHandle>>,
    traffic_persistence: AsyncMutex<Option<TrafficPersistence>>,
    lifecycle_lock: AsyncMutex<()>,
    codex_pid_before_restart: Mutex<Option<u32>>,
    activation_baseline: AtomicU64,
    activation_baseline_request_id: AtomicU64,
    pending_verification_models: Mutex<HashSet<String>>,
    shutting_down: AtomicBool,
    session_names: Arc<SessionNameCache>,
    session_name_task: AsyncMutex<Option<SessionNameTask>>,
    self_ref: OnceLock<Weak<Self>>,
}

#[derive(Debug)]
struct TrafficPersistence {
    stop: oneshot::Sender<()>,
    task: tauri::async_runtime::JoinHandle<io::Result<()>>,
}

impl AppRuntime {
    pub(crate) fn new(paths: RuntimePaths) -> Arc<Self> {
        let preferences = load_preferences(&paths.preferences_path());
        let mut status = AppStatus::starting(&preferences);
        let session_names = SessionNameCache::new(codex_database_path(&paths.config_path));
        let home = paths
            .config_path
            .parent()
            .and_then(Path::parent)
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        let catalog_status = catalog::starting_status(&home);
        status.catalog = catalog_status.clone();
        let metrics = Arc::new(Metrics::load_traffic(&paths.traffic_path()));
        let runtime = Arc::new(Self {
            paths,
            compression_enabled: Arc::new(AtomicBool::new(preferences.compression_enabled)),
            websocket_enabled: Arc::new(AtomicBool::new(preferences.websocket_enabled)),
            preferences: Mutex::new(preferences),
            status: RwLock::new(status),
            catalog: Mutex::new(catalog_status),
            catalog_write_lock: AsyncMutex::new(()),
            catalog_last_sync_ms: AtomicU64::new(0),
            catalog_sync_running: AtomicBool::new(false),
            metrics,
            managed: Mutex::new(None),
            proxy: AsyncMutex::new(None),
            traffic_persistence: AsyncMutex::new(None),
            lifecycle_lock: AsyncMutex::new(()),
            codex_pid_before_restart: Mutex::new(None),
            activation_baseline: AtomicU64::new(0),
            activation_baseline_request_id: AtomicU64::new(0),
            pending_verification_models: Mutex::new(HashSet::new()),
            shutting_down: AtomicBool::new(false),
            session_names,
            session_name_task: AsyncMutex::new(None),
            self_ref: OnceLock::new(),
        });
        let _ = runtime.self_ref.set(Arc::downgrade(&runtime));
        runtime
    }

    #[allow(clippy::too_many_lines)]
    pub(crate) async fn initialize(&self) {
        let _guard = self.lifecycle_lock.lock().await;
        if self.shutting_down.load(Ordering::Relaxed) {
            return;
        }
        if let Err(error) = self.recover_model_settings_journal() {
            self.block(&error);
            return;
        }
        self.start_session_name_task().await;
        self.start_traffic_persistence().await;
        if self.proxy.lock().await.is_some() {
            return;
        }
        self.update_status(|status| {
            status.config_state = "starting".to_owned();
            status.config_message = "正在检查 Codex 配置".to_owned();
        });

        let recovery_path = self.paths.recovery_path();
        if let Err(error) = self.recover_stale_config(&recovery_path) {
            self.block(&error.to_string());
            return;
        }
        let handoff_path = self.paths.session_handoff_path();
        let (session_handoff, handoff_error) = load_session_handoff(&handoff_path);

        let check = match preflight(&self.paths.config_path) {
            Ok(check) => check,
            Err(error) => {
                if let ConfigError::InsecureUpstream {
                    provider,
                    upstream,
                    ai_cove,
                } = &error
                {
                    self.update_status(|status| {
                        status.service_healthy = false;
                        status.config_state = "needs_https".to_owned();
                        status.config_message = error.to_string();
                        status.endpoint = "—".to_owned();
                        status.provider.clone_from(provider);
                        status.upstream.clone_from(upstream);
                        status.ai_cove_upstream = *ai_cove;
                        status.codex_state = "checking".to_owned();
                        status.restart_required = false;
                        status.ai_cove_upstream_fix_available = false;
                    });
                } else {
                    let ai_cove_fix_available = matches!(&error, ConfigError::LoopbackUpstream);
                    self.block(&error.to_string());
                    if ai_cove_fix_available {
                        self.update_status(|status| status.ai_cove_upstream_fix_available = true);
                    }
                }
                return;
            }
        };

        let home = self
            .paths
            .config_path
            .parent()
            .and_then(Path::parent)
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        match catalog::ensure_catalog(
            &home,
            &self.paths.config_path,
            &self.paths.catalog_recovery_path(),
        ) {
            Ok(catalog) => {
                *lock_mutex(&self.catalog) = catalog.clone();
                self.catalog_last_sync_ms
                    .store(unix_time_ms(), Ordering::Relaxed);
                self.update_status(|status| status.catalog = catalog);
            }
            Err(error) => {
                self.update_status(|status| {
                    status.catalog.state =
                        if matches!(error, catalog::CatalogError::OwnershipConflict) {
                            "conflict".to_owned()
                        } else {
                            "error".to_owned()
                        };
                    status.catalog.restart_required = false;
                    status.catalog.loaded = false;
                });
            }
        }
        let upstream_url = match self.configured_upstream(&check) {
            Ok(upstream) => upstream,
            Err(error) => {
                self.block(&error.to_string());
                return;
            }
        };
        let upstream = upstream_url.as_str().to_owned();
        let ai_cove = upstream_compatibility(&upstream_url) == UpstreamCompatibility::AiCove;
        self.update_status(|status| {
            status.provider.clone_from(&check.provider);
            status.upstream.clone_from(&upstream);
            status.original_upstream = check.upstream.as_str().to_owned();
            status.ai_cove_upstream = ai_cove;
        });

        let forced = self.has_upstream_override();
        if !ai_cove && !forced && !self.non_ai_cove_confirmed(&upstream) {
            self.update_status(|status| {
                status.config_state = "warning".to_owned();
                status.config_message =
                    "当前上游不是 AI Cove，配置可能不生效或发生错误；确认后才能继续".to_owned();
            });
            return;
        }

        let (proxy, managed) = match self
            .start_managed_proxy(&check, upstream_url, &recovery_path)
            .await
        {
            Ok(result) => result,
            Err(error) => {
                self.block(&error);
                return;
            }
        };
        let endpoint = proxy.endpoint().to_owned();
        let websocket_enabled = self.websocket_enabled.load(Ordering::Relaxed);

        self.remember_port(&endpoint);
        *lock_mutex(&self.managed) = Some(managed);
        *self.proxy.lock().await = Some(proxy);
        let codex_pid = codex_desktop_process_id();
        self.apply_codex_activation(
            endpoint,
            session_handoff.as_ref(),
            &check,
            websocket_enabled,
            codex_pid,
        );
        if let Some(error) = handoff_error {
            self.report_session_handoff_error(&error);
        }
        if let Some(error) = clear_session_handoff(&handoff_path) {
            self.report_session_handoff_error(&error);
        }
    }

    pub(crate) async fn status(&self) -> AppStatus {
        self.refresh_ownership().await;
        self.verify_codex_restart().await;
        let _ = self.connection_snapshot().await;
        let metrics = self.metrics.snapshot();
        let traffic = self.metrics.traffic_snapshot();
        let catalog_request_verified = {
            let baseline_request_id = self.activation_baseline_request_id.load(Ordering::Relaxed);
            let mut pending = lock_mutex(&self.pending_verification_models);
            let verified = pending
                .iter()
                .filter(|model| {
                    traffic.recent_requests.iter().any(|request| {
                        request.id > baseline_request_id
                            && request.is_successful_responses_for(model)
                    })
                })
                .cloned()
                .collect::<HashSet<_>>();
            pending.retain(|model| !verified.contains(model));
            !verified.is_empty() && pending.is_empty()
        };
        let waiting_for_request = read_lock(&self.status).codex_state == "waiting_request";
        if metrics.successful_responses > self.activation_baseline.load(Ordering::Relaxed)
            && waiting_for_request
            && read_lock(&self.status).config_state == "managed"
        {
            self.update_status(|status| {
                status.codex_state = "active".to_owned();
                status.restart_required = false;
                status.config_message = "已观察到本次配置后的成功 Responses 请求".to_owned();
            });
        }
        let catalog_update = if catalog_request_verified {
            let mut catalog = lock_mutex(&self.catalog);
            if catalog.loaded && !catalog.request_verified {
                catalog.request_verified = true;
                Some(catalog.clone())
            } else {
                None
            }
        } else {
            None
        };
        if let Some(catalog) = catalog_update {
            self.update_status(|status| status.catalog = catalog);
        }
        let mut status = read_lock(&self.status).clone();
        self.refresh_transport_status(&mut status).await;
        status.requests = metrics.requests;
        status.raw_bytes = metrics.raw_bytes;
        status.sent_bytes = metrics.sent_bytes;
        status.compression_verified = status.compression_enabled && metrics.compression_verified;
        status.websocket_verified = status.websocket_enabled && metrics.websocket_verified;
        status.websocket_zstd_verified =
            status.websocket_enabled && metrics.websocket_zstd_verified;
        status.websocket_handshakes = metrics.websocket_handshakes;
        status.websocket_raw_bytes = metrics.websocket_raw_bytes;
        status.websocket_sent_bytes = metrics.websocket_sent_bytes;
        status.websocket_messages = metrics.websocket_messages;
        status.http_fallbacks = metrics.http_fallbacks;
        status.hybrid_ws = metrics.hybrid_ws;
        status.hybrid_cold_start_http = metrics.hybrid_cold_start_http;
        status.hybrid_recovery_http = metrics.hybrid_recovery_http;
        status.hybrid_policy_http = metrics.hybrid_policy_http;
        status.hybrid_capability_http = metrics.hybrid_capability_http;
        status.hybrid_large_request_http = metrics.hybrid_large_request_http;
        status.direct_http = metrics.direct_http;
        self.session_names
            .observe_requests(&traffic.recent_requests)
            .await;
        self.session_names
            .observe_hints(&self.metrics.take_session_name_hints())
            .await;
        status.recent_requests = traffic.recent_requests;
        status.session_names = self.session_names.snapshot().await;
        status.traffic_windows = traffic.windows;
        if status.websocket_state != "conflict" {
            status.websocket_state = if !status.websocket_enabled {
                "disabled".to_owned()
            } else if metrics.websocket_active > 0 {
                "connected".to_owned()
            } else if metrics.websocket_verified {
                "closed".to_owned()
            } else if metrics.websocket_failures > 0 {
                "failed".to_owned()
            } else {
                "waiting".to_owned()
            };
        }
        status.compression_ratio = if metrics.raw_bytes == 0 {
            0.0
        } else {
            let saved = metrics.raw_bytes.saturating_sub(metrics.sent_bytes);
            let basis_points = saved.saturating_mul(10_000) / metrics.raw_bytes;
            f64::from(u32::try_from(basis_points).unwrap_or_default()) / 100.0
        };
        status
    }

    pub(crate) async fn reset_route_metrics(&self) -> io::Result<()> {
        self.metrics.reset_route_metrics();
        persist_traffic_once(Arc::clone(&self.metrics), self.paths.traffic_path(), false).await
    }

    async fn refresh_transport_status(&self, status: &mut AppStatus) {
        let capability_models = lock_mutex(&self.catalog)
            .models
            .iter()
            .map(|model| model.slug.clone())
            .collect::<Vec<_>>();
        let values = {
            let proxy = self.proxy.lock().await;
            proxy.as_ref().map(|proxy| {
                proxy.refresh_capabilities(&capability_models);
                (
                    proxy.prewarm_state(),
                    proxy.model_policy_status(),
                    proxy.capability_statuses(),
                    proxy.capability_reason(),
                )
            })
        };
        let Some((
            prewarm_state,
            model_policy,
            transport_capabilities,
            transport_capability_reason,
        )) = values
        else {
            status.prewarm_state = "disabled".to_owned();
            return;
        };
        status.prewarm_state = prewarm_state;
        status.model_policy = model_policy;
        status.transport_capabilities = transport_capabilities;
        status.transport_capability_reason = transport_capability_reason;
    }

    pub(crate) async fn connection_snapshot(&self) -> ConnectionSnapshot {
        let proxy = self.proxy.lock().await;
        let snapshot = match proxy.as_ref() {
            Some(proxy) => proxy.connection_snapshot().await,
            None => ConnectionSnapshot::default(),
        };
        drop(proxy);
        self.session_names.observe_connections(&snapshot).await;
        snapshot
    }

    pub(crate) async fn update_model_policy(
        &self,
        update: ModelPolicyUpdate,
    ) -> Result<ModelPolicyStatus, String> {
        self.proxy
            .lock()
            .await
            .as_ref()
            .ok_or_else(|| "Turbo 代理尚未启动".to_owned())?
            .update_model_policy(update)
    }

    pub(crate) fn set_compression(&self, enabled: bool) {
        self.compression_enabled.store(enabled, Ordering::Relaxed);
        self.metrics.reset_compression_verification();
        let preferences = {
            let mut preferences = lock_mutex(&self.preferences);
            preferences.compression_enabled = enabled;
            preferences.clone()
        };
        let _ = save_preferences(&self.paths.preferences_path(), &preferences);
        self.update_status(|status| {
            status.compression_enabled = enabled;
            status.compression_verified = false;
            if enabled {
                status.config_message = "压缩已开启，等待首次 JSON 请求验证".to_owned();
            }
        });
    }

    pub(crate) fn set_websocket(&self, enabled: bool) -> Result<(), crate::config::ConfigError> {
        {
            let mut managed_guard = lock_mutex(&self.managed);
            let managed = managed_guard.as_mut().ok_or_else(|| {
                crate::config::ConfigError::InvalidManagedState("当前 Provider".to_owned())
            })?;
            set_managed_websocket(managed, enabled, &self.paths.recovery_path())?;
            drop(managed_guard);
        }
        self.websocket_enabled.store(enabled, Ordering::Relaxed);
        self.metrics.reset_websocket_verification();
        let preferences = {
            let mut preferences = lock_mutex(&self.preferences);
            preferences.websocket_enabled = enabled;
            preferences.clone()
        };
        let _ = save_preferences(&self.paths.preferences_path(), &preferences);
        let codex_pid = codex_desktop_process_id();
        let (codex_state, restart_required, config_message) =
            self.prepare_codex_activation(true, codex_pid);
        self.update_status(|status| {
            status.websocket_enabled = enabled;
            status.websocket_verified = false;
            status.websocket_zstd_verified = false;
            status.websocket_state = if enabled {
                "waiting".to_owned()
            } else {
                "disabled".to_owned()
            };
            status.codex_state = codex_state.to_owned();
            status.restart_required = restart_required;
            status.desktop_restarted = false;
            status.config_message = config_message.to_owned();
        });
        Ok(())
    }

    pub(crate) fn autostart_initialized(&self) -> bool {
        lock_mutex(&self.preferences).autostart_initialized
    }

    pub(crate) fn set_autostart_state(&self, enabled: bool, initialized: bool) {
        let preferences = {
            let mut preferences = lock_mutex(&self.preferences);
            preferences.autostart_initialized = initialized;
            preferences.clone()
        };
        let _ = save_preferences(&self.paths.preferences_path(), &preferences);
        self.update_status(|status| status.autostart_enabled = enabled);
    }

    pub(crate) fn set_dock_state(&self, visible: bool) {
        let preferences = {
            let mut preferences = lock_mutex(&self.preferences);
            preferences.dock_visible = visible;
            preferences.dock_initialized = true;
            preferences.clone()
        };
        let _ = save_preferences(&self.paths.preferences_path(), &preferences);
        self.update_status(|status| status.dock_visible = visible);
    }

    pub(crate) fn dock_visible(&self) -> bool {
        lock_mutex(&self.preferences).dock_visible
    }

    pub(crate) fn dock_initialized(&self) -> bool {
        lock_mutex(&self.preferences).dock_initialized
    }

    pub(crate) fn mark_desktop_restarting(&self) {
        self.update_status(|status| {
            status.codex_state = "restarting".to_owned();
            status.restart_required = true;
            status.desktop_restarted = false;
            status.config_message = "正在等待 Codex 完成退出并重新启动".to_owned();
        });
    }

    pub(crate) fn mark_desktop_restart_failed(&self, message: &str) {
        self.update_status(|status| {
            status.codex_state = "restart_failed".to_owned();
            status.restart_required = true;
            status.desktop_restarted = false;
            status.config_message = message.to_owned();
        });
    }

    pub(crate) fn mark_desktop_restarted(&self, pid: Option<u32>) {
        self.reset_activation_baseline();
        *lock_mutex(&self.codex_pid_before_restart) = pid;
        self.update_status(|status| {
            status.codex_state = "waiting_request".to_owned();
            status.desktop_restarted = true;
            status.restart_required = false;
            status.config_message = "已检测到 Codex 重新启动，等待首次请求验证".to_owned();
        });
        let catalog_update = {
            let mut catalog = lock_mutex(&self.catalog);
            if catalog.restart_required {
                catalog.restart_required = false;
                catalog.loaded = true;
                Some(catalog.clone())
            } else {
                None
            }
        };
        if let Some(catalog) = catalog_update {
            self.update_status(|status| status.catalog = catalog);
        }
        self.refresh_catalog();
    }

    #[allow(clippy::unused_async)]
    pub(crate) async fn model_catalog(&self) -> CatalogStatus {
        self.refresh_catalog();
        lock_mutex(&self.catalog).clone()
    }

    #[allow(clippy::unused_async)]
    pub(crate) async fn update_model_catalog(
        &self,
        updates: Vec<CatalogModelUpdate>,
        expected_revision: String,
    ) -> Result<CatalogStatus, catalog::CatalogError> {
        let _write_guard = self.catalog_write_lock.lock().await;
        let home = self
            .paths
            .config_path
            .parent()
            .and_then(Path::parent)
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        let current = catalog::update_catalog(
            &home,
            &self.paths.config_path,
            &self.paths.catalog_recovery_path(),
            &updates,
            &expected_revision,
        )?;
        self.set_pending_verification_models(updates.iter().map(|update| update.slug.clone()));
        *lock_mutex(&self.catalog) = current.clone();
        self.update_status(|status| status.catalog = current.clone());
        Ok(current)
    }

    #[allow(clippy::unused_async)]
    pub(crate) async fn save_model_catalog(
        &self,
        models: Vec<CatalogModel>,
        expected_revision: String,
    ) -> Result<CatalogStatus, catalog::CatalogError> {
        let _write_guard = self.catalog_write_lock.lock().await;
        let home = self
            .paths
            .config_path
            .parent()
            .and_then(Path::parent)
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        let current = catalog::save_catalog_models(
            &home,
            &self.paths.config_path,
            &self.paths.catalog_recovery_path(),
            &models,
            &expected_revision,
        )?;
        self.set_pending_verification_models(models.iter().map(|model| model.slug.clone()));
        *lock_mutex(&self.catalog) = current.clone();
        self.update_status(|status| status.catalog = current.clone());
        Ok(current)
    }

    fn prepare_model_settings_journal(
        &self,
        home: &Path,
        expected_revision: &str,
    ) -> Result<ModelSettingsJournalContext, String> {
        let fixed_path = catalog::fixed_catalog_path(home);
        let before = fs::read(&fixed_path).map_err(|_| "catalog_snapshot_failed".to_owned())?;
        let metadata_before = catalog::read_metadata(home);
        let policy_path = self.paths.model_policy_path();
        let journal_path = self.paths.model_settings_journal_path();
        let journal = ModelSettingsJournal {
            version: MODEL_SETTINGS_JOURNAL_VERSION,
            catalog_before_revision: expected_revision.to_owned(),
            catalog_before: before.clone(),
            catalog_after_revision: None,
            metadata_before: metadata_before.clone(),
            metadata_after: None,
            policy_before: read_optional_file(&policy_path).map_err(|error| error.to_string())?,
            policy_after_revision: None,
            policy_after: None,
            committed: false,
        };
        write_model_settings_journal(&journal_path, &journal)
            .map_err(|error| format!("model_settings_journal_failed:{error}"))?;
        Ok(ModelSettingsJournalContext {
            before,
            metadata_before,
            policy_path,
            journal_path,
            journal,
        })
    }

    async fn rollback_model_settings_catalog(
        &self,
        home: &Path,
        context: &ModelSettingsJournalContext,
        catalog: CatalogStatus,
        error: String,
    ) -> ModelSettingsSaveStatus {
        let rollback = catalog::restore_catalog_bytes(
            home,
            &self.paths.config_path,
            &self.paths.catalog_recovery_path(),
            &catalog.revision,
            &context.before,
        );
        if let Ok(restored) = rollback {
            self.set_pending_verification_models(std::iter::empty());
            if let Err(metadata_error) = catalog::save_metadata(home, &context.metadata_before) {
                return ModelSettingsSaveStatus {
                    catalog: restored,
                    model_policy: self.current_model_policy().await,
                    rolled_back: false,
                    partial_failure: true,
                    error: Some(format!(
                        "model_settings_partial_failure:{error};metadata_rollback:{metadata_error}"
                    )),
                };
            }
            let _ = remove_file_if_present(&context.journal_path);
            *lock_mutex(&self.catalog) = restored.clone();
            self.update_status(|status| status.catalog = restored);
            return ModelSettingsSaveStatus {
                catalog: self.model_catalog().await,
                model_policy: self.current_model_policy().await,
                rolled_back: true,
                partial_failure: false,
                error: Some(format!("model_settings_rollback:{error}")),
            };
        }
        self.set_pending_verification_models(std::iter::empty());
        ModelSettingsSaveStatus {
            catalog,
            model_policy: self.current_model_policy().await,
            rolled_back: false,
            partial_failure: true,
            error: Some(format!("model_settings_partial_failure:{error}")),
        }
    }

    fn complete_model_settings_journal(
        context: &mut ModelSettingsJournalContext,
    ) -> Result<(), String> {
        context.journal.policy_after_revision = read_optional_file(&context.policy_path)
            .map_err(|error| error.to_string())?
            .map(|bytes| catalog::revision(&bytes));
        context.journal.committed = true;
        write_model_settings_journal(&context.journal_path, &context.journal)
            .map_err(|error| format!("model_settings_journal_failed:{error}"))?;
        remove_file_if_present(&context.journal_path)
            .map_err(|error| format!("model_settings_journal_cleanup_failed:{error}"))
    }

    pub(crate) async fn save_model_settings(
        &self,
        models: Vec<CatalogModel>,
        expected_revision: String,
        policy: ModelPolicyUpdate,
        removed_slugs: Vec<String>,
    ) -> Result<ModelSettingsSaveStatus, String> {
        let _write_guard = self.catalog_write_lock.lock().await;
        let home = self
            .paths
            .config_path
            .parent()
            .and_then(Path::parent)
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        let mut context = self.prepare_model_settings_journal(&home, &expected_revision)?;
        let (catalog_after_revision, metadata_after) = match catalog::preview_catalog_models(
            &home,
            &self.paths.config_path,
            &self.paths.catalog_recovery_path(),
            &models,
            &expected_revision,
            &removed_slugs,
        ) {
            Ok(preview) => preview,
            Err(error) => {
                let _ = remove_file_if_present(&context.journal_path);
                return Err(error.to_string());
            }
        };
        context.journal.catalog_after_revision = Some(catalog_after_revision);
        context.journal.metadata_after = Some(metadata_after);
        context.journal.policy_after = Some(journal_policy(&policy));
        if let Err(error) = write_model_settings_journal(&context.journal_path, &context.journal) {
            let _ = remove_file_if_present(&context.journal_path);
            return Err(format!("model_settings_journal_failed:{error}"));
        }
        let catalog = match catalog::save_catalog_models_with_removals(
            &home,
            &self.paths.config_path,
            &self.paths.catalog_recovery_path(),
            &models,
            &expected_revision,
            &removed_slugs,
        ) {
            Ok(catalog) => catalog,
            Err(error) => {
                let catalog_restored = fs::read(catalog::fixed_catalog_path(&home))
                    .is_ok_and(|bytes| bytes == context.before);
                let metadata_restored = catalog::read_metadata(&home) == context.metadata_before;
                if catalog_restored && metadata_restored {
                    let _ = remove_file_if_present(&context.journal_path);
                    return Err(error.to_string());
                }
                let catalog = match catalog::read_catalog(
                    &home,
                    &self.paths.config_path,
                    &self.paths.catalog_recovery_path(),
                    true,
                    false,
                    false,
                ) {
                    Ok(catalog) => catalog,
                    Err(_) => self.model_catalog().await,
                };
                self.set_pending_verification_models(std::iter::empty());
                return Ok(ModelSettingsSaveStatus {
                    catalog,
                    model_policy: self.current_model_policy().await,
                    rolled_back: false,
                    partial_failure: true,
                    error: Some(format!("model_settings_partial_failure:{error}")),
                });
            }
        };
        let model_policy = match self.update_model_policy(policy).await {
            Ok(status) => status,
            Err(error) => {
                return Ok(self
                    .rollback_model_settings_catalog(&home, &context, catalog, error)
                    .await);
            }
        };
        if let Err(error) = Self::complete_model_settings_journal(&mut context) {
            return Ok(ModelSettingsSaveStatus {
                catalog,
                model_policy,
                rolled_back: false,
                partial_failure: true,
                error: Some(format!("model_settings_journal_failed:{error}")),
            });
        }
        self.set_pending_verification_models(models.iter().map(|model| model.slug.clone()));
        *lock_mutex(&self.catalog) = catalog.clone();
        self.update_status(|status| status.catalog = catalog.clone());
        Ok(ModelSettingsSaveStatus {
            catalog,
            model_policy,
            rolled_back: false,
            partial_failure: false,
            error: None,
        })
    }

    fn recover_model_settings_journal(&self) -> Result<(), String> {
        let journal_path = self.paths.model_settings_journal_path();
        let bytes = match fs::read(&journal_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(format!("无法读取模型设置恢复记录：{error}")),
        };
        let journal: ModelSettingsJournal = serde_json::from_slice(&bytes)
            .map_err(|error| format!("模型设置恢复记录无效：{error}"))?;
        if journal.version != MODEL_SETTINGS_JOURNAL_VERSION {
            return Err("模型设置恢复记录版本不受支持".to_owned());
        }
        let home = self
            .paths
            .config_path
            .parent()
            .and_then(Path::parent)
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        let fixed_path = catalog::fixed_catalog_path(&home);
        let current_catalog =
            fs::read(&fixed_path).map_err(|error| format!("无法读取待恢复的模型目录：{error}"))?;
        let current_catalog_revision = catalog::revision(&current_catalog);
        let catalog_before = current_catalog_revision == journal.catalog_before_revision;
        let catalog_after = journal
            .catalog_after_revision
            .as_deref()
            .is_some_and(|revision| revision == current_catalog_revision);
        if !catalog_before && !catalog_after {
            return Err("模型目录在联合保存恢复期间发生外部修改，请人工确认".to_owned());
        }

        let metadata_current = catalog::read_metadata(&home);
        let metadata_before = metadata_current == journal.metadata_before;
        let metadata_after = journal
            .metadata_after
            .as_ref()
            .is_some_and(|metadata| metadata_current == *metadata);
        if !metadata_before && !metadata_after {
            return Err("模型目录元数据在联合保存恢复期间发生外部修改，请人工确认".to_owned());
        }

        let policy_path = self.paths.model_policy_path();
        let current_policy = read_optional_file(&policy_path)
            .map_err(|error| format!("无法读取待恢复的传输策略：{error}"))?;
        let policy_before = optional_revision(current_policy.as_deref())
            == optional_revision(journal.policy_before.as_deref());
        let policy_after = journal
            .policy_after_revision
            .as_deref()
            .is_some_and(|revision| {
                optional_revision(current_policy.as_deref()).as_deref() == Some(revision)
            })
            || journal.policy_after.as_ref().is_some_and(|expected| {
                current_policy
                    .as_deref()
                    .and_then(parse_journal_policy)
                    .is_some_and(|current| current == *expected)
            });
        if !policy_before && !policy_after {
            return Err("传输策略在联合保存恢复期间发生外部修改，请人工确认".to_owned());
        }

        if journal.committed {
            return remove_file_if_present(&journal_path)
                .map_err(|error| format!("无法清理模型设置恢复记录：{error}"));
        }
        if catalog_after {
            catalog::restore_catalog_bytes(
                &home,
                &self.paths.config_path,
                &self.paths.catalog_recovery_path(),
                &current_catalog_revision,
                &journal.catalog_before,
            )
            .map_err(|error| format!("无法恢复模型目录：{error}"))?;
        }
        if catalog_after || metadata_after {
            catalog::save_metadata(&home, &journal.metadata_before)
                .map_err(|error| format!("无法恢复模型目录元数据：{error}"))?;
        }
        if policy_after {
            restore_optional_file(&policy_path, journal.policy_before.as_deref())
                .map_err(|error| format!("无法恢复传输策略：{error}"))?;
        }
        remove_file_if_present(&journal_path)
            .map_err(|error| format!("无法清理模型设置恢复记录：{error}"))
    }

    async fn current_model_policy(&self) -> ModelPolicyStatus {
        self.proxy.lock().await.as_ref().map_or_else(
            || ModelPolicyStatus {
                default_transport: "auto".to_owned(),
                models: std::collections::HashMap::new(),
                reason: Some("Turbo 代理尚未启动".to_owned()),
            },
            ProxyHandle::model_policy_status,
        )
    }

    fn discovery_target(&self) -> Result<Url, catalog_discovery::DiscoveryError> {
        let managed = lock_mutex(&self.managed).clone();
        let check = if let Some(managed) = managed.as_ref() {
            managed
                .original_preflight()
                .map_err(|_| catalog_discovery::DiscoveryError::InvalidUpstream)?
        } else {
            preflight(&self.paths.config_path)
                .map_err(|_| catalog_discovery::DiscoveryError::InvalidUpstream)?
        };
        let upstream = self
            .configured_upstream(&check)
            .map_err(|_| catalog_discovery::DiscoveryError::InvalidUpstream)?;
        let compatibility = if self.has_upstream_override() {
            upstream_compatibility(&upstream)
        } else {
            check.compatibility
        };
        if compatibility != UpstreamCompatibility::AiCove {
            return Err(catalog_discovery::DiscoveryError::InvalidUpstream);
        }
        Ok(upstream)
    }

    pub(crate) async fn discover_model_catalog(
        &self,
    ) -> Result<DiscoveryResult, catalog_discovery::DiscoveryError> {
        let upstream = self.discovery_target()?;
        let headers = effective_auth_headers(Some(&self.paths.config_path))
            .ok_or(catalog_discovery::DiscoveryError::MissingCredentials)?;
        let mut result = catalog_discovery::fetch(
            &reqwest::Client::new(),
            &upstream,
            &headers,
            env!("CARGO_PKG_VERSION"),
        )
        .await?;
        let existing = lock_mutex(&self.catalog)
            .models
            .iter()
            .map(|model| (model.slug.clone(), model.max_context_window))
            .collect::<std::collections::HashMap<_, _>>();
        for model in &mut result.models {
            if let (Some(current), Some(discovered)) = (
                existing.get(&model.slug).copied().flatten(),
                model.max_context_window,
            ) && current != discovered
            {
                let conservative_max = current.min(discovered);
                model.max_context_window = Some(conservative_max);
                model.context_window = model
                    .context_window
                    .map(|context| context.min(conservative_max));
                model
                    .field_sources
                    .insert("maxContextWindow".to_owned(), "冲突".to_owned());
                model.conflicts.push(format!(
                    "max_context_window: 已配置 {current}，上游报告 {discovered}"
                ));
            }
        }
        let metadata = CatalogMetadata {
            source_url: Some(result.source_url.clone()),
            source_version: result.source_version.clone(),
            fetched_at: Some(result.fetched_at.clone()),
            etag: result.etag.clone(),
            metadata_path: Some(
                catalog::fixed_catalog_path(
                    &self
                        .paths
                        .config_path
                        .parent()
                        .and_then(Path::parent)
                        .map_or_else(|| PathBuf::from("."), Path::to_path_buf),
                )
                .with_file_name("ai_cove_turbo.metadata.json")
                .display()
                .to_string(),
            ),
            field_sources: result
                .models
                .iter()
                .map(|model| (model.slug.clone(), model.field_sources.clone()))
                .collect(),
            root_source_digest: None,
            root_seen_slugs: Vec::new(),
            root_removed_slugs: Vec::new(),
            root_available: false,
            root_unavailable_reason: None,
            root_unavailable_at: None,
            root_source_type: None,
            root_codex_version: None,
            root_client_version: None,
            root_binary_digest: None,
            root_last_read_at: None,
            root_last_synced_at: None,
            root_template_source_slug: None,
            root_template_source_digest: None,
            root_field_digests: BTreeMap::new(),
            user_overrides: BTreeMap::new(),
            conflicts: result
                .models
                .iter()
                .filter(|model| !model.conflicts.is_empty())
                .map(|model| (model.slug.clone(), model.conflicts.clone()))
                .collect(),
        };
        catalog::save_metadata(
            &self
                .paths
                .config_path
                .parent()
                .and_then(Path::parent)
                .map_or_else(|| PathBuf::from("."), Path::to_path_buf),
            &metadata,
        )
        .map_err(|_| catalog_discovery::DiscoveryError::MetadataWrite)?;
        let mut status = lock_mutex(&self.catalog).clone();
        status.metadata = metadata;
        self.update_status(|app_status| app_status.catalog.metadata = status.metadata.clone());
        Ok(result)
    }

    #[allow(clippy::unused_async)]
    pub(crate) async fn restore_model_catalog(
        &self,
    ) -> Result<CatalogStatus, catalog::CatalogError> {
        let home = self
            .paths
            .config_path
            .parent()
            .and_then(Path::parent)
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        let current = catalog::restore_catalog(
            &home,
            &self.paths.config_path,
            &self.paths.catalog_recovery_path(),
        )?;
        *lock_mutex(&self.catalog) = current.clone();
        self.update_status(|status| status.catalog = current.clone());
        Ok(current)
    }

    #[allow(clippy::unused_async)]
    pub(crate) async fn reclaim_model_catalog(
        &self,
    ) -> Result<CatalogStatus, catalog::CatalogError> {
        let home = self
            .paths
            .config_path
            .parent()
            .and_then(Path::parent)
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        let current = catalog::reclaim_catalog(
            &home,
            &self.paths.config_path,
            &self.paths.catalog_recovery_path(),
        )?;
        *lock_mutex(&self.catalog) = current.clone();
        self.update_status(|status| status.catalog = current.clone());
        Ok(current)
    }

    pub(crate) async fn verify_codex_restart(&self) {
        if !matches!(
            read_lock(&self.status).codex_state.as_str(),
            "restart_required" | "waiting_start" | "restart_failed"
        ) {
            return;
        }
        let previous = *lock_mutex(&self.codex_pid_before_restart);
        let current = tauri::async_runtime::spawn_blocking(codex_desktop_process_id)
            .await
            .ok()
            .flatten();
        if codex_restart_observed(previous, current) {
            self.mark_desktop_restarted(current);
        }
    }

    pub(crate) fn set_update_status(&self, state: &str, message: &str, progress: u8) {
        self.update_status(|status| {
            status.update_state = state.to_owned();
            status.update_message = message.to_owned();
            status.update_progress = progress;
        });
    }

    pub(crate) async fn confirm_non_ai_cove(&self) {
        let upstream = read_lock(&self.status).upstream.clone();
        if upstream == "—" {
            return;
        }
        let preferences = {
            let mut preferences = lock_mutex(&self.preferences);
            preferences.confirmed_non_ai_cove_upstream = Some(upstream);
            preferences.clone()
        };
        let _ = save_preferences(&self.paths.preferences_path(), &preferences);
        self.initialize().await;
    }

    fn has_upstream_override(&self) -> bool {
        lock_mutex(&self.preferences).upstream_override.is_some()
    }

    fn configured_upstream(&self, check: &Preflight) -> Result<Url, ConfigError> {
        lock_mutex(&self.preferences)
            .upstream_override
            .as_deref()
            .map_or_else(|| Ok(check.upstream.clone()), validate_upstream_override)
    }

    async fn start_managed_proxy(
        &self,
        check: &Preflight,
        upstream: Url,
        recovery_path: &Path,
    ) -> Result<(ProxyHandle, ManagedConfig), String> {
        let ai_cove = upstream_compatibility(&upstream) == UpstreamCompatibility::AiCove;
        let proxy = start_proxy_with_policy(
            ProxyOptions {
                upstream,
                compression_enabled: Arc::clone(&self.compression_enabled),
                websocket_enabled: Arc::clone(&self.websocket_enabled),
                ai_cove_private_websocket_zstd: ai_cove,
                metrics: Arc::clone(&self.metrics),
                preferred_ports: self.preferred_ports(),
                max_request_body_bytes: 128 * 1024 * 1024,
            },
            Some(self.paths.model_policy_path()),
            Some(self.paths.config_path.clone()),
            true,
        )
        .await
        .map_err(|error| error.to_string())?;
        let endpoint = proxy.endpoint().to_owned();
        let websocket_enabled = self.websocket_enabled.load(Ordering::Relaxed);
        let managed = match take_over(check, &endpoint, websocket_enabled, recovery_path) {
            Ok(managed) => managed,
            Err(error) => {
                proxy.stop().await;
                return Err(error.to_string());
            }
        };
        Ok((proxy, managed))
    }

    pub(crate) async fn set_upstream_override(&self, raw: &str) -> Result<(), String> {
        let upstream = validate_upstream_override(raw).map_err(|error| error.to_string())?;
        let _guard = self.lifecycle_lock.lock().await;
        if self.shutting_down.load(Ordering::Relaxed) {
            return Err("Turbo 正在退出，无法切换上游".to_owned());
        }

        let managed = lock_mutex(&self.managed).clone();
        let (check, previous_endpoint) = if let Some(managed) = managed.as_ref() {
            if managed_ownership(managed).map_err(|error| error.to_string())?
                != ManagedOwnership::Owned
            {
                return Err("Codex 配置已被外部修改，请重新接管后再切换上游".to_owned());
            }
            (
                managed
                    .original_preflight()
                    .map_err(|error| error.to_string())?,
                read_lock(&self.status).endpoint.clone(),
            )
        } else {
            (
                preflight(&self.paths.config_path).map_err(|error| error.to_string())?,
                "—".to_owned(),
            )
        };
        let upstream_text = upstream.as_str().to_owned();
        let ai_cove = upstream_compatibility(&upstream) == UpstreamCompatibility::AiCove;
        let preferences = {
            let mut preferences = lock_mutex(&self.preferences);
            preferences.upstream_override = Some(upstream_text.clone());
            if !ai_cove {
                preferences.confirmed_non_ai_cove_upstream = Some(upstream_text.clone());
            }
            preferences.clone()
        };
        save_preferences(&self.paths.preferences_path(), &preferences)
            .map_err(|error| error.to_string())?;
        self.update_status(|status| {
            status.config_state = "starting".to_owned();
            status.config_message = "正在切换强制上游".to_owned();
            status.upstream = upstream_text.clone();
            status.original_upstream = check.upstream.as_str().to_owned();
            status.ai_cove_upstream = ai_cove;
            status.service_healthy = false;
        });

        let old_proxy = self.proxy.lock().await.take();
        if let Some(proxy) = old_proxy {
            proxy.stop().await;
        }
        let recovery_path = self.paths.recovery_path();
        let (proxy, managed) = match self
            .start_managed_proxy(&check, upstream, &recovery_path)
            .await
        {
            Ok(result) => result,
            Err(error) => {
                self.block(&error);
                return Err(error);
            }
        };
        let endpoint = proxy.endpoint().to_owned();
        let endpoint_changed = previous_endpoint != "—" && previous_endpoint != endpoint;
        self.remember_port(&endpoint);
        *lock_mutex(&self.managed) = Some(managed);
        *self.proxy.lock().await = Some(proxy);
        let codex_pid = codex_desktop_process_id();
        self.reset_activation_baseline();
        *lock_mutex(&self.codex_pid_before_restart) = codex_pid;
        let websocket_enabled = self.websocket_enabled.load(Ordering::Relaxed);
        self.update_status(|status| {
            status.service_healthy = true;
            status.endpoint = endpoint;
            status.provider = check.provider.clone();
            status.upstream = upstream_text.clone();
            status.original_upstream = check.upstream.as_str().to_owned();
            status.ai_cove_upstream = ai_cove;
            status.config_state = "managed".to_owned();
            status.config_message = "强制上游已生效，等待新的请求验证".to_owned();
            status.codex_state = if codex_pid.is_none() {
                "waiting_start".to_owned()
            } else if endpoint_changed {
                "restart_required".to_owned()
            } else {
                "waiting_request".to_owned()
            };
            status.restart_required = endpoint_changed && codex_pid.is_some();
            status.desktop_restarted = false;
            status.websocket_enabled = websocket_enabled;
            status.websocket_state = if websocket_enabled {
                "waiting".to_owned()
            } else {
                "disabled".to_owned()
            };
            status.websocket_verified = false;
            status.websocket_zstd_verified = false;
            status.compression_verified = false;
        });
        Ok(())
    }

    pub(crate) async fn set_ai_cove_upstream(&self) -> Result<(), ConfigError> {
        replace_loopback_upstream(&self.paths.config_path)?;
        self.update_status(|status| {
            status.ai_cove_upstream_fix_available = false;
            status.config_state = "starting".to_owned();
            status.config_message = format!("已设置 AI Cove 上游 {AI_COVE_UPSTREAM}，正在重新接管");
        });
        self.initialize().await;
        Ok(())
    }

    pub(crate) async fn retry_takeover(&self) -> Result<(), String> {
        self.initialize().await;
        {
            let status = read_lock(&self.status);
            if matches!(
                status.config_state.as_str(),
                "blocked" | "error" | "conflict" | "needs_https"
            ) {
                return Err(status.config_message.clone());
            }
        }
        Ok(())
    }

    pub(crate) async fn resume_after_failed_update(&self) {
        self.shutting_down.store(false, Ordering::Relaxed);
        self.initialize().await;
    }

    pub(crate) async fn shutdown(&self) -> Result<(), ConfigError> {
        let _guard = self.lifecycle_lock.lock().await;
        self.shutting_down.store(true, Ordering::Relaxed);
        self.stop_session_name_task().await;
        let managed = lock_mutex(&self.managed).clone();
        let recovery_path = self.paths.recovery_path();
        let handoff_path = self.paths.session_handoff_path();
        let session_handoff = session_handoff_for_shutdown(
            managed.as_ref(),
            &read_lock(&self.status),
            codex_desktop_process_id(),
        );
        if let Some(managed) = managed {
            match restore(&managed, &recovery_path) {
                Ok(RestoreOutcome::Restored) => {
                    let handoff_error = session_handoff.as_ref().map_or_else(
                        || clear_session_handoff(&handoff_path),
                        |handoff| write_session_handoff(&handoff_path, handoff).err(),
                    );
                    lock_mutex(&self.managed).take();
                    self.update_status(|status| {
                        status.config_state = "restored".to_owned();
                        status.config_message = "Codex 配置已恢复".to_owned();
                    });
                    if let Some(error) = handoff_error {
                        self.report_session_handoff_error(&error);
                    }
                }
                Ok(RestoreOutcome::NoRecord) => {
                    let handoff_error = clear_session_handoff(&handoff_path);
                    lock_mutex(&self.managed).take();
                    self.update_status(|status| {
                        status.config_state = "restored".to_owned();
                        status.config_message = "Codex 配置已恢复".to_owned();
                    });
                    if let Some(error) = handoff_error {
                        self.report_session_handoff_error(&error);
                    }
                }
                Ok(RestoreOutcome::Conflict) => {
                    let handoff_error = clear_session_handoff(&handoff_path);
                    lock_mutex(&self.managed).take();
                    self.update_status(|status| {
                        status.config_state = "conflict".to_owned();
                        status.config_message = "外部配置已取得所有权，Turbo 未覆盖该值".to_owned();
                    });
                    if let Some(error) = handoff_error {
                        self.report_session_handoff_error(&error);
                    }
                }
                Err(error) => {
                    self.shutting_down.store(false, Ordering::Relaxed);
                    self.update_status(|status| {
                        status.config_state = "error".to_owned();
                        status.config_message =
                            format!("恢复 Codex 配置失败，Turbo 继续运行：{error}");
                    });
                    return Err(error);
                }
            }
        }
        if self.paths.catalog_recovery_path().exists() {
            let home = self
                .paths
                .config_path
                .parent()
                .and_then(Path::parent)
                .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
            let restored = catalog::restore_catalog(
                &home,
                &self.paths.config_path,
                &self.paths.catalog_recovery_path(),
            )
            .map_err(|error| ConfigError::Write(std::io::Error::other(error.to_string())))?;
            *lock_mutex(&self.catalog) = restored.clone();
            self.update_status(|status| status.catalog = restored);
        }
        let proxy = self.proxy.lock().await.take();
        if let Some(proxy) = proxy {
            proxy.stop().await;
        }
        self.stop_traffic_persistence().await?;
        self.update_status(|status| status.service_healthy = false);
        Ok(())
    }

    async fn start_traffic_persistence(&self) {
        let mut persistence = self.traffic_persistence.lock().await;
        if persistence.is_some() {
            return;
        }
        let (stop, stopped) = oneshot::channel();
        let metrics = Arc::clone(&self.metrics);
        let path = self.paths.traffic_path();
        let task = tauri::async_runtime::spawn(persist_traffic(metrics, path, stopped));
        *persistence = Some(TrafficPersistence { stop, task });
    }

    async fn start_session_name_task(&self) {
        let mut task = self.session_name_task.lock().await;
        if task.is_none() {
            *task = Some(self.session_names.start());
        }
    }

    async fn stop_session_name_task(&self) {
        let task = self.session_name_task.lock().await.take();
        if let Some(task) = task {
            task.stop().await;
        }
    }

    async fn stop_traffic_persistence(&self) -> Result<(), ConfigError> {
        let persistence = self.traffic_persistence.lock().await.take();
        if let Some(persistence) = persistence {
            let _ = persistence.stop.send(());
            if matches!(persistence.task.await, Ok(Ok(()))) {
                return Ok(());
            }
        }
        persist_traffic_once(Arc::clone(&self.metrics), self.paths.traffic_path(), false)
            .await
            .map_err(ConfigError::TrafficWrite)
    }

    async fn refresh_ownership(&self) {
        if self.shutting_down.load(Ordering::Relaxed) {
            return;
        }
        let managed = lock_mutex(&self.managed).clone();
        let Some(managed) = managed else {
            return;
        };
        match managed_ownership(&managed) {
            Ok(ManagedOwnership::Owned) => {}
            Ok(ManagedOwnership::BaseUrlLost) => {
                let _ = restore(&managed, &self.paths.recovery_path());
                lock_mutex(&self.managed).take();
                let proxy = self.proxy.lock().await.take();
                if let Some(proxy) = proxy {
                    proxy.stop().await;
                }
                self.update_status(|status| {
                    status.service_healthy = false;
                    status.config_state = "conflict".to_owned();
                    status.config_message =
                        "检测到外部修改，Turbo 已停止接管且不会覆盖当前 base_url".to_owned();
                    status.restart_required = false;
                });
            }
            Ok(ManagedOwnership::WebSocketLost) => {
                let mut managed_guard = lock_mutex(&self.managed);
                if let Some(managed) = managed_guard.as_mut() {
                    let _ = relinquish_websocket(managed, &self.paths.recovery_path());
                }
                drop(managed_guard);
                self.update_status(|status| {
                    status.config_state = "conflict".to_owned();
                    status.websocket_state = "conflict".to_owned();
                    status.websocket_verified = false;
                    status.config_message = "检测到外部修改 supports_websockets；HTTP 通道继续运行，Turbo 不再覆盖该字段".to_owned();
                    status.restart_required = false;
                });
            }
            Err(error) => {
                self.update_status(|status| {
                    status.config_state = "error".to_owned();
                    status.config_message =
                        format!("读取 Codex 配置失败，Turbo 保持当前通道：{error}");
                });
            }
        }
    }

    fn block(&self, message: &str) {
        self.update_status(|status| {
            status.service_healthy = false;
            status.config_state = "blocked".to_owned();
            status.config_message = message.to_owned();
            status.endpoint = "—".to_owned();
            status.codex_state = "checking".to_owned();
            status.restart_required = false;
            status.ai_cove_upstream_fix_available = false;
        });
    }

    fn preferred_ports(&self) -> Vec<u16> {
        let mut ports = Vec::with_capacity(3);
        let last_port = lock_mutex(&self.preferences).last_port;
        if let Some(port) = last_port {
            ports.push(port);
        }
        if !ports.contains(&DEFAULT_PORT) {
            ports.push(DEFAULT_PORT);
        }
        ports.push(0);
        ports
    }

    fn remember_port(&self, endpoint: &str) {
        let port = Url::parse(endpoint).ok().and_then(|url| url.port());
        let Some(port) = port else {
            return;
        };
        let preferences = {
            let mut preferences = lock_mutex(&self.preferences);
            preferences.last_port = Some(port);
            preferences.clone()
        };
        let _ = save_preferences(&self.paths.preferences_path(), &preferences);
    }

    fn non_ai_cove_confirmed(&self, upstream: &str) -> bool {
        lock_mutex(&self.preferences)
            .confirmed_non_ai_cove_upstream
            .as_deref()
            == Some(upstream)
    }

    fn apply_codex_activation(
        &self,
        endpoint: String,
        session_handoff: Option<&SessionHandoff>,
        check: &Preflight,
        websocket_enabled: bool,
        codex_pid: Option<u32>,
    ) {
        let config_changed = config_requires_codex_restart(
            session_handoff,
            check,
            &endpoint,
            websocket_enabled,
            codex_pid,
        );
        let (codex_state, restart_required, config_message) =
            self.prepare_codex_activation(config_changed, codex_pid);
        self.update_status(|status| {
            status.service_healthy = true;
            status.endpoint = endpoint;
            status.config_state = "managed".to_owned();
            status.config_message = config_message.to_owned();
            status.codex_state = codex_state.to_owned();
            status.restart_required = restart_required;
            status.desktop_restarted = false;
            status.websocket_enabled = websocket_enabled;
            status.websocket_state = if websocket_enabled {
                "waiting".to_owned()
            } else {
                "disabled".to_owned()
            };
        });
    }

    fn prepare_codex_activation(
        &self,
        config_changed: bool,
        codex_pid: Option<u32>,
    ) -> (&'static str, bool, &'static str) {
        self.reset_activation_baseline();
        *lock_mutex(&self.codex_pid_before_restart) = codex_pid;
        initial_codex_state(config_changed, codex_pid)
    }

    fn reset_activation_baseline(&self) {
        self.activation_baseline.store(
            self.metrics.snapshot().successful_responses,
            Ordering::Relaxed,
        );
        let request_id = self
            .metrics
            .traffic_snapshot()
            .recent_requests
            .last()
            .map_or(0, |request| request.id);
        self.activation_baseline_request_id
            .store(request_id, Ordering::Relaxed);
    }

    fn set_pending_verification_models(&self, models: impl IntoIterator<Item = String>) {
        let mut pending = lock_mutex(&self.pending_verification_models);
        pending.clear();
        pending.extend(models);
    }

    fn recover_stale_config(&self, recovery_path: &Path) -> Result<StaleRecovery, ConfigError> {
        let recovery = recover_stale(recovery_path)?;
        if recovery.outcome == RestoreOutcome::Conflict {
            self.update_status(|status| {
                status.config_message = "检测到外部配置修改，已放弃旧接管值".to_owned();
            });
        }
        Ok(recovery)
    }

    fn update_status(&self, update: impl FnOnce(&mut AppStatus)) {
        update(&mut write_lock(&self.status));
    }

    pub(crate) fn refresh_catalog(&self) {
        let now = unix_time_ms();
        let previous = self.catalog_last_sync_ms.load(Ordering::Relaxed);
        if !catalog_sync_due(previous, now) {
            return;
        }
        if self.catalog_sync_running.swap(true, Ordering::AcqRel) {
            return;
        }
        self.catalog_last_sync_ms.store(now, Ordering::Relaxed);
        let Some(runtime) = self.self_ref.get().and_then(Weak::upgrade) else {
            self.catalog_sync_running.store(false, Ordering::Release);
            return;
        };
        let _ = tauri::async_runtime::spawn_blocking(move || {
            runtime.refresh_catalog_sync();
            runtime.catalog_sync_running.store(false, Ordering::Release);
        });
    }

    fn refresh_catalog_sync(&self) {
        let Ok(_write_guard) = self.catalog_write_lock.try_lock() else {
            return;
        };
        let current = lock_mutex(&self.catalog).clone();
        if current.state != "owned" {
            return;
        }
        let home = self
            .paths
            .config_path
            .parent()
            .and_then(Path::parent)
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        match catalog::sync_catalog(
            &home,
            &self.paths.config_path,
            &self.paths.catalog_recovery_path(),
            &current.revision,
            current.restart_required,
            current.loaded,
            current.request_verified,
        ) {
            Ok(updated) => {
                if updated.revision != current.revision {
                    self.set_pending_verification_models(
                        updated.models.iter().map(|model| model.slug.clone()),
                    );
                }
                *lock_mutex(&self.catalog) = updated.clone();
                self.update_status(|status| status.catalog = updated);
            }
            Err(error) => {
                let mut blocked = current;
                blocked.state = if matches!(error, catalog::CatalogError::OwnershipConflict) {
                    "conflict".to_owned()
                } else {
                    "error".to_owned()
                };
                blocked.restart_required = false;
                blocked.loaded = false;
                *lock_mutex(&self.catalog) = blocked.clone();
                self.update_status(|status| status.catalog = blocked);
            }
        }
    }

    fn report_session_handoff_error(&self, error: &ConfigError) {
        self.update_status(|status| {
            status.config_message =
                format!("Turbo 会话续接记录失败，下次启动将要求重启 Codex：{error}");
        });
    }
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or(u64::MAX)
}

fn catalog_sync_due(last_probe_ms: u64, now_ms: u64) -> bool {
    last_probe_ms == 0 || now_ms.saturating_sub(last_probe_ms) >= CATALOG_SYNC_MIN_INTERVAL_MS
}

fn load_session_handoff(handoff_path: &Path) -> (Option<SessionHandoff>, Option<ConfigError>) {
    match read_session_handoff(handoff_path) {
        Ok(handoff) => (handoff, None),
        Err(error) => (None, Some(error)),
    }
}

fn clear_session_handoff(handoff_path: &Path) -> Option<ConfigError> {
    remove_session_handoff(handoff_path).err()
}

fn session_handoff_for_shutdown(
    managed: Option<&ManagedConfig>,
    status: &AppStatus,
    codex_pid: Option<u32>,
) -> Option<SessionHandoff> {
    let managed = managed?;
    let codex_pid = codex_pid?;
    (!status.restart_required
        && matches!(status.codex_state.as_str(), "waiting_request" | "active"))
    .then(|| SessionHandoff::new(managed, codex_pid))
    .flatten()
}

fn config_requires_codex_restart(
    session_handoff: Option<&SessionHandoff>,
    check: &Preflight,
    endpoint: &str,
    websocket_enabled: bool,
    codex_pid: Option<u32>,
) -> bool {
    !session_handoff.is_some_and(|handoff| {
        handoff.matches_effective_config(check, endpoint, websocket_enabled, codex_pid)
    })
}

const fn initial_codex_state(
    config_changed: bool,
    codex_pid: Option<u32>,
) -> (&'static str, bool, &'static str) {
    if codex_pid.is_none() {
        return (
            "waiting_start",
            false,
            "本地服务已就绪，等待 Codex 启动后加载配置",
        );
    }
    if config_changed {
        return (
            "restart_required",
            true,
            "本地服务已就绪，需要重启 Codex 加载新配置",
        );
    }
    (
        "waiting_request",
        false,
        "Codex 已加载相同配置，等待首次真实请求验证",
    )
}

async fn persist_traffic(
    metrics: Arc<Metrics>,
    path: PathBuf,
    mut stopped: oneshot::Receiver<()>,
) -> io::Result<()> {
    let mut save_interval = tokio::time::interval(TRAFFIC_SAVE_INTERVAL);
    let mut compact_interval = tokio::time::interval(TRAFFIC_COMPACT_INTERVAL);
    save_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    compact_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    save_interval.tick().await;
    compact_interval.tick().await;
    loop {
        let compact = tokio::select! {
            _ = &mut stopped => break,
            _ = save_interval.tick() => false,
            _ = compact_interval.tick() => true,
        };
        let _ = persist_traffic_once(Arc::clone(&metrics), path.clone(), compact).await;
    }
    persist_traffic_once(metrics, path, false).await
}

async fn persist_traffic_once(
    metrics: Arc<Metrics>,
    path: PathBuf,
    compact: bool,
) -> io::Result<()> {
    tauri::async_runtime::spawn_blocking(move || {
        if compact {
            metrics.compact_traffic(&path)
        } else {
            metrics.save_traffic(&path)
        }
    })
    .await
    .map_err(io::Error::other)?
}

fn load_preferences(path: &Path) -> Preferences {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn read_optional_file(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn optional_revision(bytes: Option<&[u8]>) -> Option<String> {
    bytes.map(catalog::revision)
}

fn journal_policy(update: &ModelPolicyUpdate) -> JournalPolicy {
    JournalPolicy {
        default_transport: update.default_transport.clone(),
        models: update
            .models
            .iter()
            .map(|(model, transport)| (model.clone(), transport.clone()))
            .collect(),
    }
}

fn parse_journal_policy(bytes: &[u8]) -> Option<JournalPolicy> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    if value.get("version").and_then(serde_json::Value::as_u64) != Some(1) {
        return None;
    }
    let object = value.as_object()?;
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "version" | "default_transport" | "models"))
    {
        return None;
    }
    let default_transport = value
        .get("default_transport")
        .and_then(serde_json::Value::as_str)?
        .to_owned();
    if !matches!(default_transport.as_str(), "auto" | "http") {
        return None;
    }
    let models = value
        .get("models")
        .and_then(serde_json::Value::as_object)?
        .iter()
        .map(|(model, entry)| {
            if model.trim().is_empty() {
                return None;
            }
            let entry_object = entry.as_object()?;
            if entry_object.keys().any(|key| key != "transport") {
                return None;
            }
            let transport = entry.get("transport").and_then(serde_json::Value::as_str)?;
            if !matches!(transport, "auto" | "http") {
                return None;
            }
            Some((model.clone(), transport.to_owned()))
        })
        .collect::<Option<BTreeMap<_, _>>>()?;
    Some(JournalPolicy {
        default_transport,
        models,
    })
}

fn write_model_settings_journal(path: &Path, journal: &ModelSettingsJournal) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(journal).map_err(io::Error::other)?;
    write_atomic_bytes(path, &bytes)
}

fn restore_optional_file(path: &Path, bytes: Option<&[u8]>) -> io::Result<()> {
    bytes.map_or_else(
        || remove_file_if_present(path),
        |bytes| write_atomic_bytes(path, bytes),
    )
}

fn remove_file_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn write_atomic_bytes(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("atomic file path has no parent"))?;
    fs::create_dir_all(parent)?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

fn save_preferences(path: &Path, preferences: &Preferences) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(preferences).map_err(std::io::Error::other)?;
    write_atomic_bytes(path, &bytes)
}

const fn codex_restart_observed(previous: Option<u32>, current: Option<u32>) -> bool {
    match (previous, current) {
        (Some(previous), Some(current)) => previous != current,
        (None, Some(_)) => true,
        (Some(_) | None, None) => false,
    }
}

#[cfg(all(not(test), target_os = "macos"))]
pub(crate) fn codex_desktop_process_id() -> Option<u32> {
    let script = r#"tell application "System Events" to get unix id of first application process whose bundle identifier is "com.openai.codex""#;
    let output = Command::new("/usr/bin/osascript")
        .args(["-e", script])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().parse().ok())
        .flatten()
}

#[cfg(all(not(test), target_os = "windows"))]
pub(crate) fn codex_desktop_process_id() -> Option<u32> {
    crate::windows_process::process_id_by_name("Codex")
}

#[cfg(any(test, not(any(target_os = "macos", target_os = "windows"))))]
pub(crate) const fn codex_desktop_process_id() -> Option<u32> {
    None
}

fn lock_mutex<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn read_lock<T>(lock: &RwLock<T>) -> RwLockReadGuard<'_, T> {
    lock.read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn write_lock<T>(lock: &RwLock<T>) -> RwLockWriteGuard<'_, T> {
    lock.write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::{error::Error, fs};

    use tempfile::tempdir;

    use crate::proxy::CapabilityTransport;

    use super::*;

    #[test]
    fn dock_default_migrates_once_then_preserves_the_saved_choice() -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let path = root.path().join("preferences.json");

        assert!(load_preferences(&path).dock_visible);
        fs::write(&path, br#"{"dockVisible":false}"#)?;
        let runtime = AppRuntime::new(RuntimePaths {
            config_path: root.path().join("config.toml"),
            data_dir: root.path().to_path_buf(),
        });
        assert!(!runtime.dock_visible());
        assert!(!runtime.dock_initialized());

        runtime.set_dock_state(true);
        assert!(runtime.dock_visible());
        assert!(runtime.dock_initialized());

        runtime.set_dock_state(false);
        let saved = load_preferences(&path);
        assert!(!saved.dock_visible);
        assert!(saved.dock_initialized);
        Ok(())
    }

    #[tokio::test]
    async fn retry_takeover_reports_a_failed_initialization() -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let runtime = AppRuntime::new(RuntimePaths {
            config_path: root.path().join("config.toml"),
            data_dir: root.path().join("data"),
        });

        let result = runtime.retry_takeover().await;

        assert!(result.is_err());
        assert_eq!(read_lock(&runtime.status).config_state, "blocked");
        runtime.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn refresh_transport_status_keeps_last_capabilities_when_proxy_unavailable() {
        let root = tempdir().expect("temporary runtime root");
        let runtime = AppRuntime::new(RuntimePaths {
            config_path: root.path().join("config.toml"),
            data_dir: root.path().join("data"),
        });
        let mut status = AppStatus::starting(&Preferences::default());
        status.transport_capabilities.insert(
            "gpt-5.6-sol".to_owned(),
            CapabilityModelStatus {
                allowed: true,
                transport: CapabilityTransport::WebSocket,
                reason_code: "ok".to_owned(),
            },
        );

        runtime.refresh_transport_status(&mut status).await;

        assert_eq!(
            status
                .transport_capabilities
                .get("gpt-5.6-sol")
                .map(|capability| capability.transport.as_str()),
            Some("websocket")
        );
    }

    #[tokio::test]
    async fn http_ai_cove_upstream_reports_the_required_configuration_change()
    -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let config_path = root.path().join("config.toml");
        fs::write(
            &config_path,
            "model_provider = \"custom\"\n\n[model_providers.custom]\nbase_url = \"http://long-api.ai-cove.com\"\n",
        )?;
        let runtime = AppRuntime::new(RuntimePaths {
            config_path,
            data_dir: root.path().join("data"),
        });

        let result = runtime.retry_takeover().await;
        let status = runtime.status().await;

        assert!(result.is_err());
        assert_eq!(status.config_state, "needs_https");
        assert_eq!(status.provider, "custom");
        assert_eq!(status.upstream, "http://long-api.ai-cove.com");
        assert!(status.config_message.contains("HTTPS"));
        runtime.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn lifecycle_exposes_health_then_restores_codex_on_shutdown() -> Result<(), Box<dyn Error>>
    {
        let root = tempdir()?;
        let config_dir = root.path().join("home/.codex");
        fs::create_dir_all(&config_dir)?;
        let config_path = config_dir.join("config.toml");
        fs::write(
            &config_path,
            r#"model_provider = "custom"

[model_providers.custom]
base_url = "https://api.ai-cove.com/v1"
supports_websockets = false
"#,
        )?;
        let runtime = AppRuntime::new(RuntimePaths {
            config_path: config_path.clone(),
            data_dir: root.path().join("data"),
        });

        runtime.initialize().await;
        let active = runtime.status().await;
        assert!(active.service_healthy);
        assert_eq!(active.config_state, "managed");
        assert_eq!(active.upstream, "https://api.ai-cove.com/v1");
        assert!(active.websocket_enabled);
        assert_eq!(active.websocket_state, "waiting");
        assert!(active.endpoint.starts_with("http://127.0.0.1:"));
        let managed_config = fs::read_to_string(&config_path)?;
        assert!(managed_config.contains(&active.endpoint));
        assert!(managed_config.contains("supports_websockets = true"));

        runtime.set_websocket(false)?;
        let websocket_off = runtime.status().await;
        assert!(!websocket_off.websocket_enabled);
        assert!(websocket_off.compression_enabled);
        assert_eq!(websocket_off.websocket_state, "disabled");
        assert!(fs::read_to_string(&config_path)?.contains("supports_websockets = false"));

        runtime.shutdown().await?;
        let restored = fs::read_to_string(&config_path)?;
        assert!(restored.contains("https://api.ai-cove.com/v1"));
        assert!(restored.contains("supports_websockets = false"));
        assert!(!restored.contains("http://127.0.0.1:"));
        Ok(())
    }

    #[test]
    fn discovery_target_follows_forced_ai_cove_upstream() -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let config_path = root.path().join("config.toml");
        let recovery = root.path().join("recovery.json");
        fs::write(
            &config_path,
            "model_provider = \"custom\"\n\n[model_providers.custom]\nbase_url = \"https://new-api.thinkervc.com/v1\"\n",
        )?;
        let runtime = AppRuntime::new(RuntimePaths {
            config_path: config_path.clone(),
            data_dir: root.path().join("data"),
        });
        let managed = take_over(
            &preflight(&config_path)?,
            "http://127.0.0.1:9/v1",
            true,
            &recovery,
        )?;
        *lock_mutex(&runtime.managed) = Some(managed);
        assert!(matches!(
            runtime.discovery_target(),
            Err(catalog_discovery::DiscoveryError::InvalidUpstream)
        ));
        lock_mutex(&runtime.preferences).upstream_override =
            Some("https://api.ai-cove.com/v1".to_owned());
        assert_eq!(
            runtime.discovery_target()?.as_str(),
            "https://api.ai-cove.com/v1"
        );
        Ok(())
    }

    #[tokio::test]
    async fn upstream_override_restarts_proxy_without_rewriting_codex_to_real_upstream()
    -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let config_dir = root.path().join("home/.codex");
        fs::create_dir_all(&config_dir)?;
        let config_path = config_dir.join("config.toml");
        let data_dir = root.path().join("data");
        fs::write(
            &config_path,
            r#"model_provider = "custom"

[model_providers.custom]
base_url = "https://api.ai-cove.com/v1"
supports_websockets = false
"#,
        )?;
        let runtime = AppRuntime::new(RuntimePaths {
            config_path: config_path.clone(),
            data_dir: data_dir.clone(),
        });

        runtime.initialize().await;
        let before = runtime.status().await;
        runtime
            .set_upstream_override("https://gateway.example/v1")
            .await?;
        let active = runtime.status().await;

        assert!(active.service_healthy);
        assert_eq!(active.upstream, "https://gateway.example/v1");
        assert_eq!(active.original_upstream, "https://api.ai-cove.com/v1");
        assert_eq!(active.endpoint, before.endpoint);
        let managed_config = fs::read_to_string(&config_path)?;
        assert!(managed_config.contains(&active.endpoint));
        assert!(!managed_config.contains("https://gateway.example/v1"));
        assert!(
            fs::read_to_string(data_dir.join("preferences.json"))?
                .contains("\"upstreamOverride\":\"https://gateway.example/v1\"")
        );

        runtime.shutdown().await?;
        let restored = fs::read_to_string(&config_path)?;
        assert!(restored.contains("https://api.ai-cove.com/v1"));
        assert!(!restored.contains("https://gateway.example/v1"));

        let restarted = AppRuntime::new(RuntimePaths {
            config_path: config_path.clone(),
            data_dir,
        });
        restarted.initialize().await;
        let persisted = restarted.status().await;
        assert!(persisted.service_healthy);
        assert_eq!(persisted.upstream, "https://gateway.example/v1");
        assert_eq!(persisted.original_upstream, "https://api.ai-cove.com/v1");
        assert!(!fs::read_to_string(&config_path)?.contains("https://gateway.example/v1"));
        restarted.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn catalog_restart_and_request_states_are_independent_from_transport_restart()
    -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let home = root.path().join("home");
        let config_dir = home.join(".codex");
        fs::create_dir_all(&config_dir)?;
        let source = root.path().join("models.json");
        fs::write(
            &source,
            r#"{"models":[{"slug":"alpha","visibility":"list","priority":1},{"slug":"beta","visibility":"list","priority":2}]}"#,
        )?;
        let config_path = config_dir.join("config.toml");
        fs::write(
            &config_path,
            format!(
                "model_provider = \"custom\"\nmodel_catalog_json = {}\n",
                toml_edit::value(source.display().to_string())
            ),
        )?;
        let runtime = AppRuntime::new(RuntimePaths {
            config_path: config_path.clone(),
            data_dir: root.path().join("data"),
        });
        let recovery = runtime.paths.catalog_recovery_path();
        let initial = catalog::ensure_catalog(&home, &config_path, &recovery)?;
        let saved = runtime
            .update_model_catalog(
                vec![
                    CatalogModelUpdate {
                        slug: "alpha".to_owned(),
                        display_name: None,
                        description: None,
                        visibility: Some("hide".to_owned()),
                        priority: Some(1),
                    },
                    CatalogModelUpdate {
                        slug: "beta".to_owned(),
                        display_name: None,
                        description: None,
                        visibility: Some("hide".to_owned()),
                        priority: Some(2),
                    },
                ],
                initial.revision,
            )
            .await?;
        assert!(saved.restart_required);
        assert!(!runtime.status().await.restart_required);

        runtime.mark_desktop_restarted(Some(42));
        let loaded = runtime.model_catalog().await;
        assert!(loaded.loaded);
        assert!(!loaded.request_verified);
        runtime
            .metrics
            .record_successful_response_for_model_for_test("beta");
        assert!(!runtime.status().await.catalog.request_verified);
        runtime
            .metrics
            .record_successful_response_for_model_for_test("alpha");
        assert!(runtime.status().await.catalog.request_verified);
        Ok(())
    }

    #[tokio::test]
    async fn joint_model_settings_save_rolls_back_after_policy_failure()
    -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let home = root.path().join("home");
        let config_dir = home.join(".codex");
        fs::create_dir_all(&config_dir)?;
        let source = root.path().join("models.json");
        fs::write(
            &source,
            r#"{"models":[{"slug":"alpha","visibility":"list","priority":1}]}"#,
        )?;
        let config_path = config_dir.join("config.toml");
        fs::write(
            &config_path,
            format!(
                "model_provider = \"custom\"\nmodel_catalog_json = {}\n",
                toml_edit::value(source.display().to_string())
            ),
        )?;
        let runtime = AppRuntime::new(RuntimePaths {
            config_path: config_path.clone(),
            data_dir: root.path().join("data"),
        });
        let recovery = runtime.paths.catalog_recovery_path();
        let initial = catalog::ensure_catalog(&home, &config_path, &recovery)?;
        let mut model = CatalogModel::basic("alpha".to_owned());
        model.context_window = Some(125_000);
        model.max_context_window = Some(250_000);
        model.supported_reasoning_levels = vec![catalog::ReasoningLevel {
            effort: "low".to_owned(),
            description: String::new(),
        }];
        model.default_reasoning_level = Some("low".to_owned());

        let result = runtime
            .save_model_settings(
                vec![model],
                initial.revision,
                ModelPolicyUpdate {
                    default_transport: "auto".to_owned(),
                    models: std::collections::HashMap::new(),
                },
                Vec::new(),
            )
            .await?;

        assert!(result.rolled_back);
        assert!(!result.partial_failure);
        assert!(result.error.is_some());
        assert_eq!(
            fs::read(catalog::fixed_catalog_path(&home))?,
            fs::read(&source)?
        );
        assert!(!runtime.paths.model_settings_journal_path().exists());
        Ok(())
    }

    #[test]
    fn persisted_joint_save_journal_restores_catalog_after_interrupted_write()
    -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let home = root.path().join("home");
        let config_dir = home.join(".codex");
        fs::create_dir_all(&config_dir)?;
        let source = root.path().join("models.json");
        fs::write(
            &source,
            r#"{"models":[{"slug":"alpha","visibility":"list","priority":1}]}"#,
        )?;
        let config_path = config_dir.join("config.toml");
        fs::write(
            &config_path,
            format!(
                "model_provider = \"custom\"\nmodel_catalog_json = {}\n",
                toml_edit::value(source.display().to_string())
            ),
        )?;
        let runtime = AppRuntime::new(RuntimePaths {
            config_path: config_path.clone(),
            data_dir: root.path().join("data"),
        });
        let recovery = runtime.paths.catalog_recovery_path();
        let initial = catalog::ensure_catalog(&home, &config_path, &recovery)?;
        let mut context = runtime.prepare_model_settings_journal(&home, &initial.revision)?;
        let changed = catalog::update_catalog(
            &home,
            &config_path,
            &recovery,
            &[CatalogModelUpdate {
                slug: "alpha".to_owned(),
                display_name: Some("Interrupted".to_owned()),
                description: None,
                visibility: None,
                priority: None,
            }],
            &initial.revision,
        )?;
        context.journal.catalog_after_revision = Some(changed.revision);
        write_model_settings_journal(&context.journal_path, &context.journal)?;

        runtime.recover_model_settings_journal()?;

        assert_eq!(
            fs::read(catalog::fixed_catalog_path(&home))?,
            fs::read(&source)?
        );
        assert!(!context.journal_path.exists());
        Ok(())
    }

    #[test]
    fn journal_policy_match_rejects_external_shape_changes() {
        let expected = JournalPolicy {
            default_transport: "auto".to_owned(),
            models: BTreeMap::from([(String::from("alpha"), String::from("http"))]),
        };
        let current = parse_journal_policy(
            br#"{"version":1,"default_transport":"auto","models":{"alpha":{"transport":"http"}}}"#,
        );
        assert_eq!(current, Some(expected));
        assert!(
            parse_journal_policy(
                br#"{"version":1,"default_transport":"auto","models":{},"unexpected":true}"#,
            )
            .is_none()
        );
    }

    #[test]
    fn joint_save_journal_restores_target_written_after_journal_persisted()
    -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let home = root.path().join("home");
        let config_dir = home.join(".codex");
        fs::create_dir_all(&config_dir)?;
        let source = root.path().join("models.json");
        fs::write(
            &source,
            r#"{"models":[{"slug":"alpha","visibility":"list","priority":1}]}"#,
        )?;
        let config_path = config_dir.join("config.toml");
        fs::write(
            &config_path,
            format!(
                "model_provider = \"custom\"\nmodel_catalog_json = {}\n",
                toml_edit::value(source.display().to_string())
            ),
        )?;
        let runtime = AppRuntime::new(RuntimePaths {
            config_path: config_path.clone(),
            data_dir: root.path().join("data"),
        });
        let recovery = runtime.paths.catalog_recovery_path();
        let initial = catalog::ensure_catalog(&home, &config_path, &recovery)?;
        let mut model = CatalogModel::basic("alpha".to_owned());
        model.context_window = Some(125_000);
        model.max_context_window = Some(250_000);
        model.supported_reasoning_levels = vec![catalog::ReasoningLevel {
            effort: "low".to_owned(),
            description: String::new(),
        }];
        model.default_reasoning_level = Some("low".to_owned());
        let (catalog_after, metadata_after) = catalog::preview_catalog_models(
            &home,
            &config_path,
            &recovery,
            std::slice::from_ref(&model),
            &initial.revision,
            &[],
        )?;
        let mut context = runtime.prepare_model_settings_journal(&home, &initial.revision)?;
        context.journal.catalog_after_revision = Some(catalog_after);
        context.journal.metadata_after = Some(metadata_after);
        context.journal.policy_after = Some(JournalPolicy {
            default_transport: "auto".to_owned(),
            models: BTreeMap::new(),
        });
        write_model_settings_journal(&context.journal_path, &context.journal)?;

        catalog::save_catalog_models(&home, &config_path, &recovery, &[model], &initial.revision)?;
        runtime.recover_model_settings_journal()?;

        assert_eq!(
            fs::read(catalog::fixed_catalog_path(&home))?,
            fs::read(&source)?
        );
        assert_eq!(catalog::read_metadata(&home), CatalogMetadata::default());
        assert!(!context.journal_path.exists());
        Ok(())
    }

    #[tokio::test]
    async fn activation_requires_a_successful_response_after_the_current_baseline()
    -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let config_path = root.path().join("config.toml");
        fs::write(
            &config_path,
            r#"model_provider = "custom"

[model_providers.custom]
base_url = "https://api.ai-cove.com/v1"
"#,
        )?;
        let runtime = AppRuntime::new(RuntimePaths {
            config_path,
            data_dir: root.path().join("data"),
        });
        runtime.metrics.record_successful_response_for_test();
        runtime.initialize().await;

        let waiting = runtime.status().await;
        assert_eq!(waiting.codex_state, "waiting_start");
        assert!(!waiting.restart_required);

        runtime.metrics.record_failed_response_for_test();
        assert_eq!(runtime.status().await.codex_state, "waiting_start");

        runtime.metrics.record_successful_response_for_test();
        assert_eq!(runtime.status().await.codex_state, "waiting_start");

        runtime.mark_desktop_restarted(Some(42));
        assert_eq!(runtime.status().await.codex_state, "waiting_request");
        runtime.metrics.record_failed_response_for_test();
        assert_eq!(runtime.status().await.codex_state, "waiting_request");
        runtime.metrics.record_successful_response_for_test();
        assert_eq!(runtime.status().await.codex_state, "active");
        runtime.shutdown().await?;
        Ok(())
    }

    #[test]
    fn initial_activation_state_distinguishes_restart_and_startup() {
        assert_eq!(initial_codex_state(true, Some(41)).0, "restart_required");
        assert!(initial_codex_state(true, Some(41)).1);
        assert_eq!(initial_codex_state(true, None).0, "waiting_start");
        assert!(!initial_codex_state(true, None).1);
        assert_eq!(initial_codex_state(false, Some(41)).0, "waiting_request");
        assert!(!initial_codex_state(false, Some(41)).1);
    }

    #[test]
    fn shutdown_handoff_requires_loaded_codex_config() -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let config_path = root.path().join("config.toml");
        let recovery_path = root.path().join("recovery.json");
        fs::write(
            &config_path,
            r#"model_provider = "custom"

[model_providers.custom]
base_url = "https://api.ai-cove.com/v1"
"#,
        )?;
        let managed = take_over(
            &preflight(&config_path)?,
            "http://127.0.0.1:44175/v1",
            true,
            &recovery_path,
        )?;
        let mut status = AppStatus::starting(&Preferences::default());
        status.codex_state = "waiting_request".to_owned();

        assert!(session_handoff_for_shutdown(Some(&managed), &status, Some(41)).is_some());
        status.restart_required = true;
        assert!(session_handoff_for_shutdown(Some(&managed), &status, Some(41)).is_none());
        status.restart_required = false;
        status.codex_state = "waiting_start".to_owned();
        assert!(session_handoff_for_shutdown(Some(&managed), &status, Some(41)).is_none());
        Ok(())
    }

    #[test]
    fn codex_restart_decision_requires_session_handoff() -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let config_path = root.path().join("config.toml");
        let recovery_path = root.path().join("recovery.json");
        fs::write(
            &config_path,
            r#"model_provider = "custom"

[model_providers.custom]
base_url = "https://api.ai-cove.com/v1"
"#,
        )?;
        let endpoint = "http://127.0.0.1:44175/v1";
        let managed = take_over(&preflight(&config_path)?, endpoint, true, &recovery_path)?;
        restore(&managed, &recovery_path)?;
        let check = preflight(&config_path)?;
        let handoff = SessionHandoff::new(&managed, 41).expect("managed config has a fingerprint");

        assert!(!config_requires_codex_restart(
            Some(&handoff),
            &check,
            endpoint,
            true,
            Some(41),
        ));
        assert!(config_requires_codex_restart(
            None,
            &check,
            endpoint,
            true,
            Some(41),
        ));
        assert!(config_requires_codex_restart(
            Some(&handoff),
            &check,
            endpoint,
            true,
            Some(42),
        ));
        Ok(())
    }

    #[tokio::test]
    async fn loopback_upstream_offers_ai_cove_repair_and_retries_takeover()
    -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let config_dir = root.path().join("home/.codex");
        fs::create_dir_all(&config_dir)?;
        let config_path = config_dir.join("config.toml");
        fs::write(
            &config_path,
            r#"model_provider = "custom"

[model_providers.custom]
base_url = "http://127.0.0.1:44175/v1"
api_key = "keep-me"
"#,
        )?;
        let runtime = AppRuntime::new(RuntimePaths {
            config_path: config_path.clone(),
            data_dir: root.path().join("data"),
        });

        runtime.initialize().await;
        let blocked = runtime.status().await;
        assert!(!blocked.service_healthy);
        assert!(blocked.ai_cove_upstream_fix_available);

        runtime.set_ai_cove_upstream().await?;
        let active = runtime.status().await;
        assert!(active.service_healthy);
        assert_eq!(active.upstream, "https://api.ai-cove.com/v1");
        assert!(!active.ai_cove_upstream_fix_available);
        assert!(fs::read_to_string(&config_path)?.contains("api_key = \"keep-me\""));

        runtime.shutdown().await?;
        Ok(())
    }

    #[tokio::test]
    async fn external_websocket_edit_keeps_http_channel_running() -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let config_dir = root.path().join("home/.codex");
        fs::create_dir_all(&config_dir)?;
        let config_path = config_dir.join("config.toml");
        fs::write(
            &config_path,
            r#"model_provider = "custom"

[model_providers.custom]
base_url = "https://api.ai-cove.com/v1"
supports_websockets = false
"#,
        )?;
        let runtime = AppRuntime::new(RuntimePaths {
            config_path: config_path.clone(),
            data_dir: root.path().join("data"),
        });
        runtime.initialize().await;
        let endpoint = runtime.status().await.endpoint;
        let source = fs::read_to_string(&config_path)?
            .replace("supports_websockets = true", "supports_websockets = false");
        fs::write(&config_path, source)?;

        let status = runtime.status().await;

        assert!(status.service_healthy);
        assert_eq!(status.endpoint, endpoint);
        assert_eq!(status.config_state, "conflict");
        assert!(status.compression_enabled);
        runtime.shutdown().await?;
        let restored = fs::read_to_string(&config_path)?;
        assert!(restored.contains("https://api.ai-cove.com/v1"));
        assert!(restored.contains("supports_websockets = false"));
        Ok(())
    }

    #[tokio::test]
    async fn shutdown_prevents_later_takeover() -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let config_dir = root.path().join("home/.codex");
        fs::create_dir_all(&config_dir)?;
        let config_path = config_dir.join("config.toml");
        fs::write(
            &config_path,
            r#"model_provider = "custom"

[model_providers.custom]
base_url = "https://api.ai-cove.com/v1"
supports_websockets = false
"#,
        )?;
        let runtime = AppRuntime::new(RuntimePaths {
            config_path: config_path.clone(),
            data_dir: root.path().join("data"),
        });
        runtime.initialize().await;

        runtime.shutdown().await?;
        runtime.initialize().await;

        let status = runtime.status().await;
        assert!(!status.service_healthy);
        assert_eq!(status.config_state, "restored");
        assert!(!fs::read_to_string(&config_path)?.contains("http://127.0.0.1:"));
        Ok(())
    }

    #[tokio::test]
    async fn ownership_read_error_keeps_proxy_running() -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let config_dir = root.path().join("home/.codex");
        fs::create_dir_all(&config_dir)?;
        let config_path = config_dir.join("config.toml");
        fs::write(
            &config_path,
            r#"model_provider = "custom"

[model_providers.custom]
base_url = "https://api.ai-cove.com/v1"
supports_websockets = false
"#,
        )?;
        let runtime = AppRuntime::new(RuntimePaths {
            config_path: config_path.clone(),
            data_dir: root.path().join("data"),
        });
        runtime.initialize().await;
        fs::remove_file(&config_path)?;

        let status = runtime.status().await;

        assert!(status.service_healthy);
        assert_eq!(status.config_state, "error");
        Ok(())
    }

    #[test]
    fn codex_restart_requires_a_new_running_process() {
        assert!(codex_restart_observed(Some(41), Some(42)));
        assert!(codex_restart_observed(None, Some(42)));
        assert!(!codex_restart_observed(Some(42), Some(42)));
        assert!(!codex_restart_observed(Some(42), None));
    }

    #[test]
    fn traffic_persistence_runs_every_thirty_seconds() {
        assert_eq!(TRAFFIC_SAVE_INTERVAL, Duration::from_secs(30));
    }

    #[test]
    fn catalog_sync_probe_is_throttled_between_intervals() {
        assert!(catalog_sync_due(0, 1));
        assert!(!catalog_sync_due(10_000, 14_999));
        assert!(catalog_sync_due(10_000, 15_000));
    }

    #[test]
    fn catalog_refresh_is_single_flight() {
        let root = tempdir().expect("temporary runtime root");
        let runtime = AppRuntime::new(RuntimePaths {
            config_path: root.path().join("config.toml"),
            data_dir: root.path().join("data"),
        });
        runtime.catalog_sync_running.store(true, Ordering::Release);

        runtime.refresh_catalog();

        assert!(runtime.catalog_sync_running.load(Ordering::Acquire));
    }

    #[test]
    fn catalog_refresh_schedules_without_waiting_for_probe() {
        let root = tempdir().expect("temporary runtime root");
        let runtime = AppRuntime::new(RuntimePaths {
            config_path: root.path().join("config.toml"),
            data_dir: root.path().join("data"),
        });
        let started = std::time::Instant::now();

        runtime.refresh_catalog();

        assert!(started.elapsed() < Duration::from_millis(250));
        assert!(runtime.catalog_last_sync_ms.load(Ordering::Acquire) > 0);
    }

    #[tokio::test]
    async fn traffic_persistence_reports_write_failures() -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let blocked_parent = root.path().join("blocked");
        fs::write(&blocked_parent, b"not a directory")?;
        let metrics = Arc::new(Metrics::default());
        metrics.record_test_traffic();

        let result =
            persist_traffic_once(metrics, blocked_parent.join("traffic.jsonl"), false).await;

        assert!(result.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn traffic_persistence_retries_after_worker_failure() -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let runtime = AppRuntime::new(RuntimePaths {
            config_path: root.path().join("config.toml"),
            data_dir: root.path().join("data"),
        });
        runtime.metrics.record_test_traffic();
        let (stop, stopped) = oneshot::channel();
        let task = tauri::async_runtime::spawn(async move {
            let _ = stopped.await;
            Err(io::Error::other("worker failed"))
        });
        *runtime.traffic_persistence.lock().await = Some(TrafficPersistence { stop, task });

        runtime.stop_traffic_persistence().await?;

        assert!(fs::read_to_string(runtime.paths.traffic_path())?.contains("/test"));
        Ok(())
    }

    #[tokio::test]
    async fn shutdown_flushes_pending_traffic() -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let config_dir = root.path().join("home/.codex");
        fs::create_dir_all(&config_dir)?;
        let config_path = config_dir.join("config.toml");
        fs::write(
            &config_path,
            r#"model_provider = "custom"

[model_providers.custom]
base_url = "https://api.ai-cove.com/v1"
supports_websockets = false
"#,
        )?;
        let data_dir = root.path().join("data");
        let runtime = AppRuntime::new(RuntimePaths {
            config_path,
            data_dir: data_dir.clone(),
        });
        runtime.initialize().await;
        runtime.metrics.record_test_traffic();

        runtime.shutdown().await?;

        assert!(fs::read_to_string(data_dir.join("traffic.jsonl"))?.contains("/test"));
        Ok(())
    }
}
