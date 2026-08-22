use std::{
    collections::HashMap,
    env, fs,
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex},
    time::SystemTime,
};

use serde::Deserialize;

const POLICY_VERSION: u64 = 1;
static LAST_GOOD: LazyLock<Mutex<HashMap<PathBuf, ModelPolicy>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Transport {
    Auto,
    Http,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct CapabilityHint {
    pub(super) expires_at: SystemTime,
    pub(super) http_available: bool,
    pub(super) responses_websocket_available: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FilePolicy {
    version: u64,
    #[serde(default = "default_transport")]
    default_transport: String,
    #[serde(default)]
    models: HashMap<String, ModelEntry>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelEntry {
    transport: String,
}

#[derive(Clone, Debug)]
pub(super) struct ModelPolicy {
    default: Transport,
    models: HashMap<String, Transport>,
}

impl Default for ModelPolicy {
    fn default() -> Self {
        Self {
            default: Transport::Auto,
            models: HashMap::new(),
        }
    }
}

impl ModelPolicy {
    pub(super) fn load_current() -> Self {
        let Some(path) = env::var_os("AI_COVE_TURBO_MODEL_POLICY")
            .map(PathBuf::from)
            .or_else(|| {
                env::var_os("AI_COVE_TURBO_APP_DATA_DIR")
                    .map(|dir| PathBuf::from(dir).join("ai_cove_turbo_model_policy.json"))
            })
        else {
            return Self::default();
        };
        Self::load_path(&path)
    }

    pub(super) fn load_path(path: &Path) -> Self {
        let mut snapshot = match LAST_GOOD.lock() {
            Ok(snapshot) => snapshot,
            Err(poisoned) => poisoned.into_inner(),
        };
        let previous = snapshot.get(path).cloned();
        let (policy, reason) = Self::load(path, previous.as_ref());
        if reason.is_none() {
            snapshot.insert(path.to_path_buf(), policy.clone());
        }
        policy
    }

    pub(super) fn load(path: &Path, previous: Option<&Self>) -> (Self, Option<String>) {
        match fs::read_to_string(path)
            .map_err(|error| error.to_string())
            .and_then(|source| Self::parse(&source).map_err(str::to_owned))
        {
            Ok(policy) => (policy, None),
            Err(reason) => (previous.cloned().unwrap_or_default(), Some(reason)),
        }
    }

    pub(super) fn parse(source: &str) -> Result<Self, &'static str> {
        let file: FilePolicy = serde_json::from_str(source).map_err(|_| "invalid_json")?;
        if file.version != POLICY_VERSION {
            return Err("unsupported_version");
        }
        let default =
            parse_transport(&file.default_transport).ok_or("invalid_default_transport")?;
        let models = file
            .models
            .into_iter()
            .map(|(slug, entry)| {
                if slug.trim().is_empty() {
                    return Err("empty_model_slug");
                }
                parse_transport(&entry.transport)
                    .map(|transport| (slug, transport))
                    .ok_or("invalid_model_transport")
            })
            .collect::<Result<HashMap<_, _>, _>>()?;
        Ok(Self { default, models })
    }

    pub(super) fn transport_for(&self, model: Option<&str>) -> Transport {
        model
            .and_then(|slug| self.models.get(slug).copied())
            .unwrap_or(self.default)
    }

    pub(super) fn transport_for_payload(&self, payload: &[u8]) -> Transport {
        let model = serde_json::from_slice::<serde_json::Value>(payload)
            .ok()
            .and_then(|value| {
                value
                    .get("model")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            });
        self.transport_for(model.as_deref())
    }

    pub(super) fn transport_for_payload_with_hint(
        &self,
        payload: &[u8],
        hint: Option<CapabilityHint>,
    ) -> Transport {
        let policy = self.transport_for_payload(payload);
        if policy == Transport::Http {
            return policy;
        }
        let Some(hint) = hint else {
            return policy;
        };
        if SystemTime::now() >= hint.expires_at
            || hint.responses_websocket_available
            || !hint.http_available
        {
            Transport::Auto
        } else {
            Transport::Http
        }
    }
}

fn default_transport() -> String {
    "auto".to_owned()
}

fn parse_transport(value: &str) -> Option<Transport> {
    match value {
        "auto" => Some(Transport::Auto),
        "http" => Some(Transport::Http),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{ModelPolicy, Transport};
    use std::fs;

    #[test]
    fn exact_model_match_defaults_to_auto() {
        let policy = ModelPolicy::parse(
            r#"{"version":1,"default_transport":"auto","models":{"gpt-http":{"transport":"http"}}}"#,
        )
        .expect("valid policy");
        assert_eq!(policy.transport_for(Some("gpt-http")), Transport::Http);
        assert_eq!(policy.transport_for(Some("gpt-http-plus")), Transport::Auto);
        assert_eq!(policy.transport_for(None), Transport::Auto);
    }

    #[test]
    fn malformed_policy_keeps_last_good_snapshot() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("ai_cove_turbo_model_policy.json");
        fs::write(&path, "{bad").expect("write malformed policy");
        let previous =
            ModelPolicy::parse(r#"{"version":1,"default_transport":"http","models":{}}"#)
                .expect("valid policy");
        let (loaded, reason) = ModelPolicy::load(&path, Some(&previous));
        assert_eq!(loaded.transport_for(None), Transport::Http);
        assert!(reason.is_some());
    }

    #[test]
    fn expired_or_unavailable_capability_hint_falls_back_to_auto() {
        let policy = ModelPolicy::default();
        let hint = super::CapabilityHint {
            expires_at: std::time::SystemTime::now() + std::time::Duration::from_secs(30),
            http_available: true,
            responses_websocket_available: false,
        };
        assert_eq!(
            policy.transport_for_payload_with_hint(br#"{"model":"gpt"}"#, Some(hint)),
            Transport::Http
        );
        let expired = super::CapabilityHint {
            expires_at: std::time::SystemTime::UNIX_EPOCH,
            ..hint
        };
        assert_eq!(
            policy.transport_for_payload_with_hint(br#"{"model":"gpt"}"#, Some(expired)),
            Transport::Auto
        );
    }

    #[test]
    fn missing_policy_uses_builtin_auto() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("missing.json");
        let (loaded, reason) = ModelPolicy::load(&path, None);
        assert_eq!(loaded.transport_for(Some("any-model")), Transport::Auto);
        assert!(reason.is_some_and(|reason| !reason.is_empty()));
    }
}
