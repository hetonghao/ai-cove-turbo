use std::{
    fmt, fs,
    io::Write,
    net::IpAddr,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;
use toml_edit::{DocumentMut, Item, value};
use url::Url;

pub(crate) const AI_COVE_UPSTREAM: &str = "https://api.ai-cove.com/v1";
const AI_COVE_HOST_SUFFIX: &str = ".ai-cove.com";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum UpstreamCompatibility {
    AiCove,
    OtherHttps,
}

#[derive(Clone, Debug)]
pub(crate) struct Preflight {
    pub(crate) config_path: PathBuf,
    pub(crate) effective_config_digest: Vec<u8>,
    pub(crate) provider: String,
    pub(crate) upstream: Url,
    pub(crate) supports_websockets: Option<bool>,
    pub(crate) compatibility: UpstreamCompatibility,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct ManagedConfig {
    config_path: PathBuf,
    provider: String,
    original_base_url: String,
    managed_base_url: String,
    #[serde(default)]
    original_supports_websockets: Option<bool>,
    #[serde(default)]
    original_effective_config_digest: Option<Vec<u8>>,
    #[serde(default)]
    managed_supports_websockets: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct SessionHandoff {
    #[serde(default)]
    handoff_fingerprint: Option<Vec<u8>>,
    codex_pid: u32,
}

impl SessionHandoff {
    pub(crate) fn new(managed: &ManagedConfig, codex_pid: u32) -> Option<Self> {
        Some(Self {
            handoff_fingerprint: Some(session_handoff_fingerprint(
                &managed.config_path,
                managed.original_effective_config_digest.as_deref()?,
                &managed.managed_base_url,
                managed.managed_supports_websockets?,
            )),
            codex_pid,
        })
    }

    pub(crate) fn matches_effective_config(
        &self,
        check: &Preflight,
        endpoint: &str,
        websocket_enabled: bool,
        codex_pid: Option<u32>,
    ) -> bool {
        codex_pid == Some(self.codex_pid)
            && self.handoff_fingerprint.as_deref()
                == Some(
                    session_handoff_fingerprint(
                        &check.config_path,
                        &check.effective_config_digest,
                        endpoint,
                        websocket_enabled,
                    )
                    .as_slice(),
                )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RestoreOutcome {
    Restored,
    Conflict,
    NoRecord,
}

#[derive(Clone, Debug)]
pub(crate) struct StaleRecovery {
    pub(crate) outcome: RestoreOutcome,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ManagedOwnership {
    Owned,
    BaseUrlLost,
    WebSocketLost,
}

#[derive(Debug)]
pub(crate) enum ConfigError {
    Missing,
    Read(std::io::Error),
    InvalidToml(toml_edit::TomlError),
    MissingProviderSelection,
    MissingProvider(String),
    MissingBaseUrl(String),
    InvalidBaseUrl(url::ParseError),
    LoopbackUpstream,
    InsecureUpstream,
    InvalidManagedEndpoint,
    Write(std::io::Error),
    TrafficWrite(std::io::Error),
    Json(serde_json::Error),
    InvalidManagedState(String),
    FieldOwnershipLost(&'static str),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => write!(formatter, "未找到默认 Codex 配置 ~/.codex/config.toml"),
            Self::Read(error) => write!(formatter, "无法读取 Codex 配置：{error}"),
            Self::InvalidToml(error) => write!(formatter, "Codex 配置 TOML 无法解析：{error}"),
            Self::MissingProviderSelection => write!(formatter, "Codex 根配置缺少 model_provider"),
            Self::MissingProvider(provider) => {
                write!(formatter, "Codex 配置缺少受管 Provider：{provider}")
            }
            Self::MissingBaseUrl(provider) => {
                write!(formatter, "受管 Provider {provider} 缺少 base_url")
            }
            Self::InvalidBaseUrl(error) => write!(formatter, "Provider base_url 无效：{error}"),
            Self::LoopbackUpstream => {
                write!(formatter, "当前上游是本机回环地址，Turbo 已阻止代理回环")
            }
            Self::InsecureUpstream => write!(formatter, "Turbo 只接管 HTTPS 上游"),
            Self::InvalidManagedEndpoint => write!(formatter, "Turbo 本地端点必须是 HTTP 回环地址"),
            Self::Write(error) => write!(formatter, "无法原子更新 Codex 配置：{error}"),
            Self::TrafficWrite(error) => write!(formatter, "无法持久化 Turbo 流量统计：{error}"),
            Self::Json(error) => write!(formatter, "Turbo 恢复记录无效：{error}"),
            Self::InvalidManagedState(provider) => {
                write!(formatter, "受管 Provider {provider} 的配置结构已改变")
            }
            Self::FieldOwnershipLost(field) => {
                write!(
                    formatter,
                    "Codex 配置中的 {field} 已被外部修改，Turbo 不会覆盖"
                )
            }
        }
    }
}

impl std::error::Error for ConfigError {}

pub(crate) fn preflight(path: &Path) -> Result<Preflight, ConfigError> {
    if !path.exists() {
        return Err(ConfigError::Missing);
    }
    let source = fs::read_to_string(path).map_err(ConfigError::Read)?;
    let document = source
        .parse::<DocumentMut>()
        .map_err(ConfigError::InvalidToml)?;
    let provider = document
        .get("model_provider")
        .and_then(toml_edit::Item::as_str)
        .ok_or(ConfigError::MissingProviderSelection)?
        .to_owned();
    let provider_table = document
        .get("model_providers")
        .and_then(|providers| providers.get(&provider))
        .and_then(toml_edit::Item::as_table_like)
        .ok_or_else(|| ConfigError::MissingProvider(provider.clone()))?;
    let raw_upstream = provider_table
        .get("base_url")
        .and_then(toml_edit::Item::as_str)
        .ok_or_else(|| ConfigError::MissingBaseUrl(provider.clone()))?;
    let upstream = Url::parse(raw_upstream).map_err(ConfigError::InvalidBaseUrl)?;
    let supports_websockets = match provider_table.get("supports_websockets") {
        Some(item) => Some(
            item.as_bool()
                .ok_or_else(|| ConfigError::InvalidManagedState(provider.clone()))?,
        ),
        None => None,
    };
    let effective_config_digest =
        digest_effective_config(&provider, &upstream, supports_websockets);

    if is_loopback(&upstream) {
        return Err(ConfigError::LoopbackUpstream);
    }
    if upstream.scheme() != "https" {
        return Err(ConfigError::InsecureUpstream);
    }

    let compatibility = if upstream.host_str().is_some_and(is_ai_cove_host) {
        UpstreamCompatibility::AiCove
    } else {
        UpstreamCompatibility::OtherHttps
    };

    Ok(Preflight {
        config_path: path.to_path_buf(),
        effective_config_digest,
        provider,
        upstream,
        supports_websockets,
        compatibility,
    })
}

pub(crate) fn set_ai_cove_upstream(path: &Path) -> Result<(), ConfigError> {
    let source = fs::read_to_string(path).map_err(ConfigError::Read)?;
    let mut document = source
        .parse::<DocumentMut>()
        .map_err(ConfigError::InvalidToml)?;
    let provider = document
        .get("model_provider")
        .and_then(Item::as_str)
        .ok_or(ConfigError::MissingProviderSelection)?
        .to_owned();
    let current = provider_base_url(&document, &provider)?;
    let current_url = Url::parse(&current).map_err(ConfigError::InvalidBaseUrl)?;
    if !is_loopback(&current_url) {
        return Err(ConfigError::FieldOwnershipLost("base_url"));
    }
    provider_table_mut(&mut document, &provider)?.insert("base_url", value(AI_COVE_UPSTREAM));
    write_atomic(path, document.to_string().as_bytes())
}

pub(crate) fn take_over(
    check: &Preflight,
    endpoint: &str,
    websocket_enabled: bool,
    recovery_path: &Path,
) -> Result<ManagedConfig, ConfigError> {
    let endpoint_url = Url::parse(endpoint).map_err(ConfigError::InvalidBaseUrl)?;
    if endpoint_url.scheme() != "http" || !is_loopback(&endpoint_url) {
        return Err(ConfigError::InvalidManagedEndpoint);
    }

    let managed = ManagedConfig {
        config_path: check.config_path.clone(),
        provider: check.provider.clone(),
        original_base_url: check.upstream.as_str().to_owned(),
        managed_base_url: endpoint.to_owned(),
        original_supports_websockets: check.supports_websockets,
        original_effective_config_digest: Some(check.effective_config_digest.clone()),
        managed_supports_websockets: Some(websocket_enabled),
    };
    write_json_atomic(recovery_path, &managed)?;

    let result = update_managed_values(
        &managed.config_path,
        &managed.provider,
        endpoint,
        websocket_enabled,
    );
    if result.is_err() {
        let _ = fs::remove_file(recovery_path);
    }
    result.map(|()| managed)
}

pub(crate) fn recover_stale(recovery_path: &Path) -> Result<StaleRecovery, ConfigError> {
    if !recovery_path.exists() {
        return Ok(StaleRecovery {
            outcome: RestoreOutcome::NoRecord,
        });
    }
    let bytes = fs::read(recovery_path).map_err(ConfigError::Read)?;
    let managed = serde_json::from_slice::<ManagedConfig>(&bytes).map_err(ConfigError::Json)?;
    let outcome = restore(&managed, recovery_path)?;
    Ok(StaleRecovery { outcome })
}

pub(crate) fn read_session_handoff(
    handoff_path: &Path,
) -> Result<Option<SessionHandoff>, ConfigError> {
    let bytes = match fs::read(handoff_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(ConfigError::Read(error)),
    };
    Ok(serde_json::from_slice(&bytes).ok())
}

pub(crate) fn write_session_handoff(
    handoff_path: &Path,
    handoff: &SessionHandoff,
) -> Result<(), ConfigError> {
    write_json_atomic(handoff_path, handoff)
}

pub(crate) fn remove_session_handoff(handoff_path: &Path) -> Result<(), ConfigError> {
    match fs::remove_file(handoff_path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(ConfigError::Write(error)),
    }
}

pub(crate) fn restore(
    managed: &ManagedConfig,
    recovery_path: &Path,
) -> Result<RestoreOutcome, ConfigError> {
    let source = fs::read_to_string(&managed.config_path).map_err(ConfigError::Read)?;
    let mut document = source
        .parse::<DocumentMut>()
        .map_err(ConfigError::InvalidToml)?;
    let base_url_owned =
        provider_base_url(&document, &managed.provider)? == managed.managed_base_url;
    let websocket_owned = managed.managed_supports_websockets.is_some_and(|value| {
        provider_websocket(&document, &managed.provider).ok() == Some(Some(value))
    });
    let websocket_conflict = managed.managed_supports_websockets.is_some() && !websocket_owned;

    if base_url_owned || websocket_owned {
        let provider_table = provider_table_mut(&mut document, &managed.provider)?;
        if base_url_owned {
            provider_table.insert("base_url", value(&managed.original_base_url));
        }
        if websocket_owned {
            match managed.original_supports_websockets {
                Some(original) => {
                    provider_table.insert("supports_websockets", value(original));
                }
                None => {
                    provider_table.remove("supports_websockets");
                }
            }
        }
        write_atomic(&managed.config_path, document.to_string().as_bytes())?;
    }
    let outcome = if base_url_owned && !websocket_conflict {
        RestoreOutcome::Restored
    } else {
        RestoreOutcome::Conflict
    };
    match fs::remove_file(recovery_path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(ConfigError::Write(error)),
    }
    Ok(outcome)
}

pub(crate) fn managed_ownership(managed: &ManagedConfig) -> Result<ManagedOwnership, ConfigError> {
    let source = fs::read_to_string(&managed.config_path).map_err(ConfigError::Read)?;
    let document = source
        .parse::<DocumentMut>()
        .map_err(ConfigError::InvalidToml)?;
    if provider_base_url(&document, &managed.provider)? != managed.managed_base_url {
        return Ok(ManagedOwnership::BaseUrlLost);
    }
    if managed.managed_supports_websockets.is_some()
        && provider_websocket(&document, &managed.provider)? != managed.managed_supports_websockets
    {
        return Ok(ManagedOwnership::WebSocketLost);
    }
    Ok(ManagedOwnership::Owned)
}

pub(crate) fn set_managed_websocket(
    managed: &mut ManagedConfig,
    enabled: bool,
    recovery_path: &Path,
) -> Result<(), ConfigError> {
    let source = fs::read_to_string(&managed.config_path).map_err(ConfigError::Read)?;
    let mut document = source
        .parse::<DocumentMut>()
        .map_err(ConfigError::InvalidToml)?;
    if provider_websocket(&document, &managed.provider)? != managed.managed_supports_websockets {
        return Err(ConfigError::FieldOwnershipLost("supports_websockets"));
    }
    let previous = managed.clone();
    managed.managed_supports_websockets = Some(enabled);
    write_json_atomic(recovery_path, managed)?;
    let result = provider_table_mut(&mut document, &managed.provider)
        .map(|provider| provider.insert("supports_websockets", value(enabled)))
        .and_then(|_| write_atomic(&managed.config_path, document.to_string().as_bytes()));
    if let Err(error) = result {
        *managed = previous;
        let _ = write_json_atomic(recovery_path, managed);
        return Err(error);
    }
    Ok(())
}

pub(crate) fn relinquish_websocket(
    managed: &mut ManagedConfig,
    recovery_path: &Path,
) -> Result<(), ConfigError> {
    managed.managed_supports_websockets = None;
    write_json_atomic(recovery_path, managed)
}

fn update_managed_values(
    path: &Path,
    provider: &str,
    base_url: &str,
    websocket_enabled: bool,
) -> Result<(), ConfigError> {
    let source = fs::read_to_string(path).map_err(ConfigError::Read)?;
    let mut document = source
        .parse::<DocumentMut>()
        .map_err(ConfigError::InvalidToml)?;
    let provider_table = provider_table_mut(&mut document, provider)?;
    provider_table.insert("base_url", value(base_url));
    provider_table.insert("supports_websockets", value(websocket_enabled));
    write_atomic(path, document.to_string().as_bytes())
}

fn provider_table_mut<'a>(
    document: &'a mut DocumentMut,
    provider: &str,
) -> Result<&'a mut dyn toml_edit::TableLike, ConfigError> {
    document
        .get_mut("model_providers")
        .and_then(|providers| providers.get_mut(provider))
        .and_then(Item::as_table_like_mut)
        .ok_or_else(|| ConfigError::InvalidManagedState(provider.to_owned()))
}

fn provider_base_url(document: &DocumentMut, provider: &str) -> Result<String, ConfigError> {
    document
        .get("model_providers")
        .and_then(|providers| providers.get(provider))
        .and_then(Item::as_table_like)
        .and_then(|provider_table| provider_table.get("base_url"))
        .and_then(Item::as_str)
        .map(str::to_owned)
        .ok_or_else(|| ConfigError::InvalidManagedState(provider.to_owned()))
}

fn digest_effective_config(
    provider: &str,
    upstream: &Url,
    supports_websockets: Option<bool>,
) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(b"turbo-codex-effective-config-v1\0");
    hasher.update(provider.as_bytes());
    hasher.update([0]);
    hasher.update(upstream.as_str().as_bytes());
    hasher.update([0]);
    hasher.update([match supports_websockets {
        None => 0,
        Some(false) => 1,
        Some(true) => 2,
    }]);
    hasher.finalize().to_vec()
}

fn session_handoff_fingerprint(
    config_path: &Path,
    effective_config_digest: &[u8],
    endpoint: &str,
    websocket_enabled: bool,
) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(b"turbo-session-handoff-v2\0");
    hasher.update(config_path.to_string_lossy().as_bytes());
    hasher.update([0]);
    hasher.update(effective_config_digest);
    hasher.update([0]);
    hasher.update(endpoint.as_bytes());
    hasher.update([u8::from(websocket_enabled)]);
    hasher.finalize().to_vec()
}

fn provider_websocket(document: &DocumentMut, provider: &str) -> Result<Option<bool>, ConfigError> {
    let provider_table = document
        .get("model_providers")
        .and_then(|providers| providers.get(provider))
        .and_then(Item::as_table_like)
        .ok_or_else(|| ConfigError::InvalidManagedState(provider.to_owned()))?;
    provider_table
        .get("supports_websockets")
        .map_or(Ok(None), |item| {
            item.as_bool()
                .map(Some)
                .ok_or_else(|| ConfigError::InvalidManagedState(provider.to_owned()))
        })
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<(), ConfigError> {
    let bytes = serde_json::to_vec(value).map_err(ConfigError::Json)?;
    write_atomic(path, &bytes)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), ConfigError> {
    let parent = path
        .parent()
        .ok_or_else(|| ConfigError::Write(std::io::Error::other("target has no parent")))?;
    fs::create_dir_all(parent).map_err(ConfigError::Write)?;
    let mut temporary = NamedTempFile::new_in(parent).map_err(ConfigError::Write)?;
    temporary.write_all(bytes).map_err(ConfigError::Write)?;
    temporary.as_file().sync_all().map_err(ConfigError::Write)?;
    if let Ok(metadata) = fs::metadata(path) {
        temporary
            .as_file()
            .set_permissions(metadata.permissions())
            .map_err(ConfigError::Write)?;
    }
    temporary
        .persist(path)
        .map_err(|error| ConfigError::Write(error.error))?;
    Ok(())
}

fn is_loopback(url: &Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<IpAddr>()
        .is_ok_and(|address| address.is_loopback())
}

fn is_ai_cove_host(host: &str) -> bool {
    let normalized = host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase();
    normalized
        .strip_suffix(AI_COVE_HOST_SUFFIX)
        .is_some_and(|prefix| !prefix.is_empty() && !prefix.ends_with('.'))
}

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn preflight_selects_only_the_root_provider() -> Result<(), Box<dyn Error>> {
        let home = tempdir()?;
        let config_dir = home.path().join(".codex");
        fs::create_dir(&config_dir)?;
        let path = config_dir.join("config.toml");
        fs::write(
            &path,
            r#"model_provider = "custom"

[model_providers.custom]
name = "AI Cove"
base_url = "https://api.ai-cove.com/v1"
supports_websockets = false

[model_providers.other]
base_url = "https://example.com/v1"
"#,
        )?;

        let result = preflight(&path)?;

        assert_eq!(result.provider, "custom");
        assert_eq!(result.upstream.as_str(), "https://api.ai-cove.com/v1");
        assert_eq!(result.compatibility, UpstreamCompatibility::AiCove);
        Ok(())
    }

    #[test]
    fn preflight_classifies_ai_cove_subdomains_by_host_boundary() -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let path = root.path().join("config.toml");
        for (base_url, expected) in [
            (
                "https://long-api.ai-cove.com/v1",
                UpstreamCompatibility::AiCove,
            ),
            ("https://API.AI-COVE.COM./v1", UpstreamCompatibility::AiCove),
            (
                "https://api.ai-cove.com.evil.example/v1",
                UpstreamCompatibility::OtherHttps,
            ),
            (
                "https://not-ai-cove.com/v1",
                UpstreamCompatibility::OtherHttps,
            ),
        ] {
            fs::write(
                &path,
                format!(
                    "model_provider = \"custom\"\n\n[model_providers.custom]\nbase_url = \"{base_url}\"\n"
                ),
            )?;
            assert_eq!(preflight(&path)?.compatibility, expected, "{base_url}");
        }
        Ok(())
    }

    #[test]
    fn takeover_and_restore_manage_base_url_and_websocket_independently()
    -> Result<(), Box<dyn Error>> {
        let home = tempdir()?;
        let config_dir = home.path().join(".codex");
        fs::create_dir(&config_dir)?;
        let config_path = config_dir.join("config.toml");
        let recovery_path = home.path().join("turbo/recovery.json");
        fs::write(
            &config_path,
            r#"# keep this comment
model_provider = "custom"
model = "gpt-5.6-luna"

[model_providers.custom]
name = "AI Cove"
base_url = "https://api.ai-cove.com/v1"
supports_websockets = false

[model_providers.other]
base_url = "https://example.com/v1"
"#,
        )?;
        let check = preflight(&config_path)?;

        let managed = take_over(&check, "http://127.0.0.1:44175/v1", true, &recovery_path)?;
        let taken_over = fs::read_to_string(&config_path)?;
        assert!(taken_over.contains("# keep this comment"));
        assert!(taken_over.contains("model = \"gpt-5.6-luna\""));
        assert!(taken_over.contains("supports_websockets = true"));
        assert!(taken_over.contains("base_url = \"https://example.com/v1\""));
        assert!(taken_over.contains("base_url = \"http://127.0.0.1:44175/v1\""));
        assert!(recovery_path.exists());

        let outcome = restore(&managed, &recovery_path)?;
        assert_eq!(outcome, RestoreOutcome::Restored);
        let restored = fs::read_to_string(&config_path)?;
        assert!(restored.contains("base_url = \"https://api.ai-cove.com/v1\""));
        assert!(restored.contains("supports_websockets = false"));
        assert!(restored.contains("base_url = \"https://example.com/v1\""));
        assert!(!recovery_path.exists());
        Ok(())
    }

    #[test]
    fn stale_recovery_restores_the_previous_managed_config() -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let config_path = root.path().join("config.toml");
        let recovery_path = root.path().join("recovery.json");
        fs::write(
            &config_path,
            r#"model_provider = "custom"

[model_providers.custom]
base_url = "https://api.ai-cove.com/v1"
supports_websockets = false
"#,
        )?;
        let endpoint = "http://127.0.0.1:44175/v1";
        take_over(&preflight(&config_path)?, endpoint, true, &recovery_path)?;

        let recovery = recover_stale(&recovery_path)?;

        assert_eq!(recovery.outcome, RestoreOutcome::Restored);
        assert!(fs::read_to_string(config_path)?.contains("https://api.ai-cove.com/v1"));
        assert!(!recovery_path.exists());
        Ok(())
    }

    #[test]
    fn session_handoff_matches_only_same_codex_and_endpoint() -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let config_path = root.path().join("config.toml");
        let recovery_path = root.path().join("recovery.json");
        fs::write(
            &config_path,
            r#"model_provider = "custom"

[model_providers.custom]
base_url = "https://api.ai-cove.com/v1"
supports_websockets = false
"#,
        )?;
        let endpoint = "http://127.0.0.1:44175/v1";
        let original = preflight(&config_path)?;
        let managed = take_over(&original, endpoint, true, &recovery_path)?;
        let handoff = SessionHandoff::new(&managed, 41).expect("managed config has a fingerprint");
        assert_eq!(restore(&managed, &recovery_path)?, RestoreOutcome::Restored);
        let restored = preflight(&config_path)?;
        assert_eq!(
            original.effective_config_digest,
            restored.effective_config_digest
        );
        let handoff_path = root.path().join("handoff.json");
        write_session_handoff(&handoff_path, &handoff)?;
        let persisted_json = fs::read_to_string(&handoff_path)?;
        assert!(!persisted_json.contains("api.ai-cove.com"));
        assert!(!persisted_json.contains("44175"));
        assert!(!persisted_json.contains("model_provider"));

        assert!(handoff.matches_effective_config(&restored, endpoint, true, Some(41)));
        let persisted = read_session_handoff(&handoff_path)?;
        assert!(persisted.as_ref().is_some_and(|handoff| {
            handoff.matches_effective_config(&restored, endpoint, true, Some(41))
        }));
        assert!(!handoff.matches_effective_config(&restored, endpoint, true, Some(42)));
        assert!(!handoff.matches_effective_config(
            &restored,
            "http://127.0.0.1:44176/v1",
            true,
            Some(41),
        ));
        assert!(!handoff.matches_effective_config(&restored, endpoint, false, Some(41)));
        let source = fs::read_to_string(&config_path)?;
        fs::write(
            &config_path,
            format!(
                "  {source}\n# Formatting and unrelated providers do not change the effective config.\n[model_providers.other]\nbase_url = \"https://example.com/v1\"\n"
            ),
        )?;
        let unchanged_config = preflight(&config_path)?;
        assert!(handoff.matches_effective_config(&unchanged_config, endpoint, true, Some(41)));
        let changed_source = fs::read_to_string(&config_path)?.replacen(
            "https://api.ai-cove.com/v1",
            "https://other.example/v1",
            1,
        );
        fs::write(&config_path, changed_source)?;
        let changed_config = preflight(&config_path)?;
        assert!(!handoff.matches_effective_config(&changed_config, endpoint, true, Some(41)));
        remove_session_handoff(&handoff_path)?;
        assert!(read_session_handoff(&handoff_path)?.is_none());
        Ok(())
    }

    #[test]
    fn invalid_session_handoff_is_ignored() -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let handoff_path = root.path().join("handoff.json");
        fs::write(&handoff_path, b"not-json")?;

        assert!(read_session_handoff(&handoff_path)?.is_none());
        Ok(())
    }

    #[test]
    fn external_base_url_edit_takes_ownership() -> Result<(), Box<dyn Error>> {
        let home = tempdir()?;
        let config_dir = home.path().join(".codex");
        fs::create_dir(&config_dir)?;
        let config_path = config_dir.join("config.toml");
        let recovery_path = home.path().join("turbo/recovery.json");
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
        fs::write(
            &config_path,
            r#"model_provider = "custom"

[model_providers.custom]
base_url = "https://external.example/v1"
"#,
        )?;

        assert_eq!(managed_ownership(&managed)?, ManagedOwnership::BaseUrlLost);
        assert_eq!(restore(&managed, &recovery_path)?, RestoreOutcome::Conflict);
        assert!(fs::read_to_string(&config_path)?.contains("https://external.example/v1"));
        assert!(!recovery_path.exists());
        Ok(())
    }

    #[test]
    fn managed_ownership_classifies_websocket_conflict() -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let config_path = root.path().join("config.toml");
        let recovery_path = root.path().join("recovery.json");
        fs::write(
            &config_path,
            r#"model_provider = "custom"

[model_providers.custom]
base_url = "https://api.ai-cove.com/v1"
supports_websockets = false
"#,
        )?;
        let managed = take_over(
            &preflight(&config_path)?,
            "http://127.0.0.1:44175/v1",
            true,
            &recovery_path,
        )?;
        let source = fs::read_to_string(&config_path)?
            .replace("supports_websockets = true", "supports_websockets = false");
        fs::write(&config_path, source)?;

        assert_eq!(
            managed_ownership(&managed)?,
            ManagedOwnership::WebSocketLost
        );
        Ok(())
    }

    #[test]
    fn restore_removes_websocket_field_that_was_originally_absent() -> Result<(), Box<dyn Error>> {
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
        assert!(fs::read_to_string(&config_path)?.contains("supports_websockets = true"));

        assert_eq!(restore(&managed, &recovery_path)?, RestoreOutcome::Restored);
        assert!(!fs::read_to_string(&config_path)?.contains("supports_websockets"));
        Ok(())
    }

    #[test]
    fn external_websocket_edit_does_not_block_base_url_restore() -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let config_path = root.path().join("config.toml");
        let recovery_path = root.path().join("recovery.json");
        fs::write(
            &config_path,
            r#"model_provider = "custom"

[model_providers.custom]
base_url = "https://api.ai-cove.com/v1"
supports_websockets = false
"#,
        )?;
        let managed = take_over(
            &preflight(&config_path)?,
            "http://127.0.0.1:44175/v1",
            true,
            &recovery_path,
        )?;
        let source = fs::read_to_string(&config_path)?
            .replace("supports_websockets = true", "supports_websockets = false");
        fs::write(&config_path, source)?;

        assert_eq!(restore(&managed, &recovery_path)?, RestoreOutcome::Conflict);
        let restored = fs::read_to_string(&config_path)?;
        assert!(restored.contains("base_url = \"https://api.ai-cove.com/v1\""));
        assert!(restored.contains("supports_websockets = false"));
        Ok(())
    }

    #[test]
    fn preflight_blocks_loopback_and_insecure_upstreams() -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let path = root.path().join("config.toml");
        for (base_url, expected) in [
            ("http://127.0.0.1:44175/v1", "回环地址"),
            ("http://example.com/v1", "HTTPS"),
        ] {
            fs::write(
                &path,
                format!(
                    "model_provider = \"custom\"\n[model_providers.custom]\nbase_url = \"{base_url}\"\n"
                ),
            )?;
            let error = preflight(&path).expect_err("must reject unsafe upstream");
            assert!(error.to_string().contains(expected));
        }
        Ok(())
    }

    #[test]
    fn set_ai_cove_upstream_preserves_provider_fields() -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let path = root.path().join("config.toml");
        fs::write(
            &path,
            r#"model_provider = "custom"

[model_providers.custom]
base_url = "http://127.0.0.1:44175/v1"
name = "AI Cove"
api_key = "keep-me"
supports_websockets = false
"#,
        )?;

        set_ai_cove_upstream(&path)?;

        let check = preflight(&path)?;
        assert_eq!(check.upstream.as_str(), "https://api.ai-cove.com/v1");
        let source = fs::read_to_string(&path)?;
        assert!(source.contains("name = \"AI Cove\""));
        assert!(source.contains("api_key = \"keep-me\""));
        assert!(source.contains("supports_websockets = false"));
        Ok(())
    }
}
