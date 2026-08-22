use std::{
    env, fs,
    path::{Path, PathBuf},
};

use axum::http::{HeaderMap, HeaderValue, header};
use toml_edit::DocumentMut;

pub(super) fn effective_auth_headers(config_path: Option<&Path>) -> Option<HeaderMap> {
    let key = env::var("OPENAI_API_KEY")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            let codex_home = env::var_os("CODEX_HOME")
                .map(PathBuf::from)
                .or_else(|| config_path.and_then(Path::parent).map(Path::to_path_buf))
                .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))?;
            let config_path =
                config_path.map_or_else(|| codex_home.join("config.toml"), Path::to_path_buf);
            resolve_api_key(&codex_home, &config_path)
        })?;
    let authorization = HeaderValue::from_str(&format!("Bearer {}", key.trim())).ok()?;
    let mut headers = HeaderMap::new();
    headers.insert(header::AUTHORIZATION, authorization);
    Some(headers)
}

fn resolve_api_key(codex_home: &Path, config_path: &Path) -> Option<String> {
    if let Some(env_key) = provider_env_key(config_path)
        && let Ok(value) = env::var(env_key)
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
