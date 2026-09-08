use std::{
    env, fs,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

use axum::http::{HeaderMap, HeaderValue, header};
use toml_edit::DocumentMut;

static AUTH_OVERRIDE: OnceLock<Mutex<Option<String>>> = OnceLock::new();
static ORIGINAL_KEY: OnceLock<Mutex<Option<String>>> = OnceLock::new();

pub(super) fn set_auth_override(key: Option<String>) {
    let restored = key.or_else(|| {
        ORIGINAL_KEY
            .get()
            .and_then(|value| value.lock().ok().and_then(|key| key.clone()))
    });
    *AUTH_OVERRIDE
        .get_or_init(|| Mutex::new(None))
        .lock()
        .expect("auth override lock") = restored;
}

pub(super) fn effective_auth_headers(config_path: Option<&Path>) -> Option<HeaderMap> {
    let codex_home = env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| config_path.and_then(Path::parent).map(Path::to_path_buf))
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))?;
    let config_path = config_path.map_or_else(|| codex_home.join("config.toml"), Path::to_path_buf);
    let original = ORIGINAL_KEY.get_or_init(|| {
        Mutex::new(resolve_api_key(&codex_home, &config_path).or_else(|| {
            env::var("OPENAI_API_KEY")
                .ok()
                .filter(|value| !value.trim().is_empty())
        }))
    });
    let key = AUTH_OVERRIDE
        .get()
        .and_then(|value| value.lock().ok().and_then(|key| key.clone()))
        .or_else(|| original.lock().ok().and_then(|key| key.clone()))?;
    let authorization = HeaderValue::from_str(&format!("Bearer {}", key.trim())).ok()?;
    let mut headers = HeaderMap::new();
    headers.insert(header::AUTHORIZATION, authorization);
    Some(headers)
}

pub(super) fn persist_api_key(config_path: &Path, key: &str) -> Result<(), String> {
    let home = config_path.parent().ok_or("Codex 配置目录不存在")?;
    let path = home.join("auth.json");
    let mut auth = fs::read_to_string(&path)
        .ok()
        .and_then(|source| serde_json::from_str::<serde_json::Value>(&source).ok())
        .unwrap_or_else(|| serde_json::json!({"auth_mode": "apiKey"}));
    auth["auth_mode"] = serde_json::Value::String("apiKey".to_owned());
    auth["OPENAI_API_KEY"] = serde_json::Value::String(key.to_owned());
    fs::write(
        &path,
        serde_json::to_vec_pretty(&auth).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("无法保存 Codex 密钥：{e}"))?;
    *ORIGINAL_KEY
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|_| "密钥状态锁已损坏".to_owned())? = Some(key.to_owned());
    Ok(())
}

fn resolve_api_key(codex_home: &Path, config_path: &Path) -> Option<String> {
    if let Some(env_key) = provider_env_key(config_path)
        && let Ok(value) = env::var(env_key)
        && !value.trim().is_empty()
    {
        return Some(value.trim().to_owned());
    }
    if let Ok(value) = env::var("OPENAI_API_KEY")
        && !value.trim().is_empty()
    {
        return Some(value.trim().to_owned());
    }
    let source = fs::read_to_string(codex_home.join("auth.json")).ok()?;
    let auth: serde_json::Value = serde_json::from_str(&source).ok()?;
    if auth
        .get("auth_mode")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|mode| mode != "apiKey")
    {
        return None;
    }
    auth.get("OPENAI_API_KEY")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(str::to_owned)
}

fn provider_env_key(config_path: &Path) -> Option<String> {
    let source = fs::read_to_string(config_path).ok()?;
    let document = source.parse::<DocumentMut>().ok()?;
    let provider = document.get("model_provider")?.as_str()?;
    document
        .get("model_providers")?
        .get(provider)?
        .get("env_key")?
        .as_str()
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::resolve_api_key;
    use std::fs;

    #[test]
    fn file_api_key_is_consumed_without_persisting_a_copy() {
        let root = tempfile::tempdir().expect("tempdir");
        let config = root.path().join("config.toml");
        fs::write(&config, "cli_auth_credentials_store = 'file'").expect("config");
        fs::write(
            root.path().join("auth.json"),
            r#"{"auth_mode":"apiKey","OPENAI_API_KEY":"test-key"}"#,
        )
        .expect("auth");
        assert_eq!(
            resolve_api_key(root.path(), &config).as_deref(),
            Some("test-key")
        );
    }

    #[test]
    fn managed_keyring_or_oauth_auth_never_becomes_a_fake_bearer_key() {
        let root = tempfile::tempdir().expect("tempdir");
        let config = root.path().join("config.toml");
        fs::write(&config, "cli_auth_credentials_store = 'auto'").expect("config");
        fs::write(
            root.path().join("auth.json"),
            r#"{"auth_mode":"chatgpt","OPENAI_API_KEY":"must-not-use"}"#,
        )
        .expect("auth");
        assert!(resolve_api_key(root.path(), &config).is_none());
    }

    #[test]
    fn current_codex_auth_shape_without_auth_mode_is_supported() {
        let root = tempfile::tempdir().expect("tempdir");
        let config = root.path().join("config.toml");
        fs::write(&config, "cli_auth_credentials_store = 'auto'").expect("config");
        fs::write(
            root.path().join("auth.json"),
            r#"{"OPENAI_API_KEY":"test-key","OPENAI_BASE_URL":"https://example.test/v1"}"#,
        )
        .expect("auth");
        assert_eq!(
            resolve_api_key(root.path(), &config).as_deref(),
            Some("test-key")
        );
    }
}
