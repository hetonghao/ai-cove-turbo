#![allow(clippy::assigning_clones)]

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard,
        atomic::{AtomicBool, Ordering},
    },
};

use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;
use tokio::sync::Mutex as AsyncMutex;
use url::Url;

use crate::{
    config::{
        AI_COVE_UPSTREAM, ConfigError, ManagedConfig, RestoreOutcome, UpstreamCompatibility,
        manages_websocket, owns_current_value, owns_websocket_value, preflight, recover_stale,
        relinquish_websocket, restore, set_ai_cove_upstream as replace_loopback_upstream,
        set_managed_websocket, take_over,
    },
    proxy::{Metrics, ProxyHandle, ProxyOptions, start_proxy},
};

const DEFAULT_PORT: u16 = 44_175;

#[derive(Clone, Debug)]
pub(crate) struct RuntimePaths {
    pub(crate) config_path: PathBuf,
    pub(crate) data_dir: PathBuf,
}

impl RuntimePaths {
    fn recovery_path(&self) -> PathBuf {
        self.data_dir.join("recovery.json")
    }

    fn preferences_path(&self) -> PathBuf {
        self.data_dir.join("preferences.json")
    }
}

#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase")]
struct Preferences {
    compression_enabled: bool,
    websocket_enabled: bool,
    autostart_initialized: bool,
    dock_visible: bool,
    last_port: Option<u16>,
    confirmed_non_ai_cove_upstream: Option<String>,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            compression_enabled: true,
            websocket_enabled: true,
            autostart_initialized: false,
            dock_visible: false,
            last_port: None,
            confirmed_non_ai_cove_upstream: None,
        }
    }
}

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
    pub(crate) ai_cove_upstream: bool,
    pub(crate) ai_cove_upstream_fix_available: bool,
    pub(crate) compression_enabled: bool,
    pub(crate) compression_verified: bool,
    pub(crate) websocket_enabled: bool,
    pub(crate) websocket_verified: bool,
    pub(crate) websocket_zstd_verified: bool,
    pub(crate) websocket_state: String,
    pub(crate) websocket_handshakes: u64,
    pub(crate) websocket_raw_bytes: u64,
    pub(crate) websocket_sent_bytes: u64,
    pub(crate) http_fallbacks: u64,
    pub(crate) autostart_enabled: bool,
    pub(crate) dock_visible: bool,
    pub(crate) dock_control_available: bool,
    pub(crate) restart_required: bool,
    pub(crate) desktop_restarted: bool,
    pub(crate) requests: u64,
    pub(crate) raw_bytes: u64,
    pub(crate) sent_bytes: u64,
    pub(crate) compression_ratio: f64,
    pub(crate) update_state: String,
    pub(crate) update_message: String,
    pub(crate) update_progress: u8,
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
            websocket_handshakes: 0,
            websocket_raw_bytes: 0,
            websocket_sent_bytes: 0,
            http_fallbacks: 0,
            autostart_enabled: true,
            dock_visible: preferences.dock_visible,
            dock_control_available: cfg!(target_os = "macos"),
            restart_required: false,
            desktop_restarted: false,
            requests: 0,
            raw_bytes: 0,
            sent_bytes: 0,
            compression_ratio: 0.0,
            update_state: "idle".to_owned(),
            update_message: "尚未检查更新".to_owned(),
            update_progress: 0,
        }
    }
}

#[derive(Debug)]
pub(crate) struct AppRuntime {
    paths: RuntimePaths,
    preferences: Mutex<Preferences>,
    status: RwLock<AppStatus>,
    compression_enabled: Arc<AtomicBool>,
    websocket_enabled: Arc<AtomicBool>,
    metrics: Arc<Metrics>,
    managed: Mutex<Option<ManagedConfig>>,
    proxy: AsyncMutex<Option<ProxyHandle>>,
    lifecycle_lock: AsyncMutex<()>,
    shutting_down: AtomicBool,
}

impl AppRuntime {
    pub(crate) fn new(paths: RuntimePaths) -> Arc<Self> {
        let preferences = load_preferences(&paths.preferences_path());
        let status = AppStatus::starting(&preferences);
        Arc::new(Self {
            paths,
            compression_enabled: Arc::new(AtomicBool::new(preferences.compression_enabled)),
            websocket_enabled: Arc::new(AtomicBool::new(preferences.websocket_enabled)),
            preferences: Mutex::new(preferences),
            status: RwLock::new(status),
            metrics: Arc::new(Metrics::default()),
            managed: Mutex::new(None),
            proxy: AsyncMutex::new(None),
            lifecycle_lock: AsyncMutex::new(()),
            shutting_down: AtomicBool::new(false),
        })
    }

    pub(crate) async fn initialize(&self) {
        let _guard = self.lifecycle_lock.lock().await;
        if self.shutting_down.load(Ordering::Relaxed) {
            return;
        }
        if self.proxy.lock().await.is_some() {
            return;
        }
        self.update_status(|status| {
            status.config_state = "starting".to_owned();
            status.config_message = "正在检查 Codex 配置".to_owned();
        });

        let recovery_path = self.paths.recovery_path();
        match recover_stale(&recovery_path) {
            Ok(RestoreOutcome::Conflict) => self.update_status(|status| {
                status.config_message = "检测到外部配置修改，已放弃旧接管值".to_owned();
            }),
            Ok(RestoreOutcome::Restored | RestoreOutcome::NoRecord) => {}
            Err(error) => {
                self.block(&error.to_string());
                return;
            }
        }

        let check = match preflight(&self.paths.config_path) {
            Ok(check) => check,
            Err(error) => {
                let ai_cove_fix_available = matches!(&error, ConfigError::LoopbackUpstream);
                self.block(&error.to_string());
                if ai_cove_fix_available {
                    self.update_status(|status| status.ai_cove_upstream_fix_available = true);
                }
                return;
            }
        };
        let upstream = check.upstream.as_str().to_owned();
        let ai_cove = check.compatibility == UpstreamCompatibility::AiCove;
        self.update_status(|status| {
            status.provider.clone_from(&check.provider);
            status.upstream.clone_from(&upstream);
            status.ai_cove_upstream = ai_cove;
        });

        if !ai_cove && !self.non_ai_cove_confirmed(&upstream) {
            self.update_status(|status| {
                status.config_state = "warning".to_owned();
                status.config_message =
                    "当前上游不是 AI Cove，配置可能不生效或发生错误；确认后才能继续".to_owned();
            });
            return;
        }

        let preferred_ports = self.preferred_ports();
        let proxy = match start_proxy(ProxyOptions {
            upstream: check.upstream.clone(),
            compression_enabled: Arc::clone(&self.compression_enabled),
            websocket_enabled: Arc::clone(&self.websocket_enabled),
            ai_cove_private_websocket_zstd: ai_cove,
            metrics: Arc::clone(&self.metrics),
            preferred_ports,
            max_request_body_bytes: 128 * 1024 * 1024,
        })
        .await
        {
            Ok(proxy) => proxy,
            Err(error) => {
                self.block(&error.to_string());
                return;
            }
        };
        let endpoint = proxy.endpoint().to_owned();
        let websocket_enabled = self.websocket_enabled.load(Ordering::Relaxed);
        let managed = match take_over(&check, &endpoint, websocket_enabled, &recovery_path) {
            Ok(managed) => managed,
            Err(error) => {
                proxy.stop().await;
                self.block(&error.to_string());
                return;
            }
        };

        self.remember_port(&endpoint);
        *lock_mutex(&self.managed) = Some(managed);
        *self.proxy.lock().await = Some(proxy);
        self.update_status(|status| {
            status.service_healthy = true;
            status.endpoint = endpoint;
            status.config_state = "managed".to_owned();
            status.config_message = "本地服务已就绪，等待 Codex 首次请求验证".to_owned();
            status.restart_required = true;
            status.desktop_restarted = false;
            status.websocket_enabled = websocket_enabled;
            status.websocket_state = if websocket_enabled {
                "waiting".to_owned()
            } else {
                "disabled".to_owned()
            };
        });
    }

    pub(crate) async fn status(&self) -> AppStatus {
        self.refresh_ownership().await;
        let metrics = self.metrics.snapshot();
        let mut status = read_lock(&self.status).clone();
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
        status.http_fallbacks = metrics.http_fallbacks;
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
        if (metrics.requests > 0 || metrics.websocket_handshakes > 0)
            && status.config_state == "managed"
        {
            status.restart_required = false;
            status.config_message = "已观察到 Codex 请求经过 Turbo".to_owned();
        }
        status
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
        self.update_status(|status| {
            status.websocket_enabled = enabled;
            status.websocket_verified = false;
            status.websocket_zstd_verified = false;
            status.websocket_state = if enabled {
                "waiting".to_owned()
            } else {
                "disabled".to_owned()
            };
            status.restart_required = true;
            status.desktop_restarted = false;
            status.config_message = if enabled {
                "WebSocket 已开启，重启 Codex 后等待首次握手验证".to_owned()
            } else {
                "WebSocket 已关闭，重启 Codex 后生效；HTTP 压缩保持独立".to_owned()
            };
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
            preferences.clone()
        };
        let _ = save_preferences(&self.paths.preferences_path(), &preferences);
        self.update_status(|status| status.dock_visible = visible);
    }

    pub(crate) fn dock_visible(&self) -> bool {
        lock_mutex(&self.preferences).dock_visible
    }

    pub(crate) fn mark_desktop_restarted(&self) {
        self.update_status(|status| {
            status.desktop_restarted = true;
            status.restart_required = false;
        });
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

    pub(crate) async fn retry_takeover(&self) {
        self.initialize().await;
    }

    pub(crate) async fn resume_after_failed_update(&self) {
        self.shutting_down.store(false, Ordering::Relaxed);
        self.initialize().await;
    }

    pub(crate) async fn shutdown(&self) -> Result<(), ConfigError> {
        let _guard = self.lifecycle_lock.lock().await;
        self.shutting_down.store(true, Ordering::Relaxed);
        let managed = lock_mutex(&self.managed).clone();
        let recovery_path = self.paths.recovery_path();
        if let Some(managed) = managed {
            match restore(&managed, &recovery_path) {
                Ok(RestoreOutcome::Restored | RestoreOutcome::NoRecord) => {
                    lock_mutex(&self.managed).take();
                    self.update_status(|status| {
                        status.config_state = "restored".to_owned();
                        status.config_message = "Codex 配置已恢复".to_owned();
                    });
                }
                Ok(RestoreOutcome::Conflict) => {
                    lock_mutex(&self.managed).take();
                    self.update_status(|status| {
                        status.config_state = "conflict".to_owned();
                        status.config_message = "外部配置已取得所有权，Turbo 未覆盖该值".to_owned();
                    });
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
        let proxy = self.proxy.lock().await.take();
        if let Some(proxy) = proxy {
            proxy.stop().await;
        }
        self.update_status(|status| status.service_healthy = false);
        Ok(())
    }

    async fn refresh_ownership(&self) {
        if self.shutting_down.load(Ordering::Relaxed) {
            return;
        }
        let managed = lock_mutex(&self.managed).clone();
        let Some(managed) = managed else {
            return;
        };
        match owns_current_value(&managed) {
            Ok(true) => {}
            Ok(false) => {
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
                return;
            }
            Err(error) => {
                self.update_status(|status| {
                    status.config_state = "error".to_owned();
                    status.config_message =
                        format!("读取 Codex 配置失败，Turbo 保持当前通道：{error}");
                });
                return;
            }
        }

        if manages_websocket(&managed) {
            match owns_websocket_value(&managed) {
                Ok(true) => {}
                Ok(false) => {
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
                Err(error) => self.update_status(|status| {
                    status.config_state = "error".to_owned();
                    status.config_message =
                        format!("读取 supports_websockets 失败，HTTP 通道继续运行：{error}");
                }),
            }
        }
    }

    fn block(&self, message: &str) {
        self.update_status(|status| {
            status.service_healthy = false;
            status.config_state = "blocked".to_owned();
            status.config_message = message.to_owned();
            status.endpoint = "—".to_owned();
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

    fn update_status(&self, update: impl FnOnce(&mut AppStatus)) {
        update(&mut write_lock(&self.status));
    }
}

fn load_preferences(path: &Path) -> Preferences {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save_preferences(path: &Path, preferences: &Preferences) -> std::io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("preferences path has no parent"))?;
    fs::create_dir_all(parent)?;
    let bytes = serde_json::to_vec(preferences).map_err(std::io::Error::other)?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    temporary.write_all(&bytes)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
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

    use super::*;

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
}
