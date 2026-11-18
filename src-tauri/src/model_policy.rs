use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
};

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use tempfile::NamedTempFile;

const POLICY_VERSION: u64 = 1;
const FAST_MODELS_FILE: &str = "ai_cove_turbo_fast_models.json";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Transport {
    Auto,
    Http,
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

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModelPolicyStatus {
    pub(crate) default_transport: String,
    pub(crate) models: HashMap<String, String>,
    pub(crate) reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ModelPolicyUpdate {
    pub(crate) default_transport: String,
    pub(crate) models: HashMap<String, String>,
}

#[derive(Debug)]
struct StoreState {
    policy: ModelPolicy,
    reason: Option<String>,
}

#[derive(Debug)]
pub(super) struct ModelPolicyStore {
    path: PathBuf,
    state: Mutex<StoreState>,
    fast_models: Mutex<BTreeSet<String>>,
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
    pub(super) fn load(path: &Path, previous: Option<&Self>) -> (Self, Option<String>) {
        let source = match fs::read_to_string(path) {
            Ok(source) => source,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return (Self::default(), None);
            }
            Err(error) => {
                return (
                    previous.cloned().unwrap_or_default(),
                    Some(error.to_string()),
                );
            }
        };
        match Self::parse(&source) {
            Ok(policy) => (policy, None),
            Err(reason) => (
                previous.cloned().unwrap_or_default(),
                Some(reason.to_owned()),
            ),
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
        let model = Self::model_from_payload(payload);
        self.transport_for(model.as_deref())
    }

    pub(super) fn model_from_payload(payload: &[u8]) -> Option<String> {
        serde_json::from_slice::<serde_json::Value>(payload)
            .ok()
            .and_then(|value| {
                value
                    .get("model")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            })
    }

    pub(super) fn model_slugs(&self) -> Vec<String> {
        let mut models = self.models.keys().cloned().collect::<Vec<_>>();
        models.sort_unstable();
        models
    }
}

impl ModelPolicyStore {
    pub(super) fn new(path: PathBuf) -> Self {
        let (policy, reason) = ModelPolicy::load(&path, None);
        let fast_models = fs::read(path.with_file_name(FAST_MODELS_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<BTreeSet<String>>(&bytes).ok())
            .unwrap_or_default()
            .into_iter()
            .filter(|slug| is_fast_model(slug))
            .collect();
        Self {
            path,
            state: Mutex::new(StoreState { policy, reason }),
            fast_models: Mutex::new(fast_models),
        }
    }

    pub(super) fn fast_models(&self) -> BTreeSet<String> {
        lock(&self.fast_models).clone()
    }

    pub(super) fn set_fast(&self, slug: String, enabled: bool) -> Result<BTreeSet<String>, String> {
        if !is_fast_model(&slug) {
            return Err("Fast 模式仅支持 GPT 模型".to_owned());
        }
        let mut current = lock(&self.fast_models);
        let mut next = current.clone();
        let changed = if enabled {
            next.insert(slug)
        } else {
            next.remove(&slug)
        };
        if changed {
            let bytes = serde_json::to_vec_pretty(&next).map_err(|error| error.to_string())?;
            write_atomic(&self.path.with_file_name(FAST_MODELS_FILE), &bytes)
                .map_err(|error| format!("无法保存 Fast 模式：{error}"))?;
            *current = next;
        }
        Ok(current.clone())
    }

    pub(super) fn apply_fast(
        &self,
        payload: &[u8],
        inherited_model: Option<&str>,
    ) -> Option<Vec<u8>> {
        if lock(&self.fast_models).is_empty() {
            return None;
        }
        // Keep nested prompt and schema values byte-for-byte while changing the top-level tier.
        let mut fields: BTreeMap<String, &RawValue> = serde_json::from_slice(payload).ok()?;
        if fields
            .get("generate")
            .is_some_and(|raw| raw.get() == "false")
        {
            return None;
        }
        let model = match fields.get("model") {
            Some(raw) => {
                let slug: std::borrow::Cow<'_, str> = serde_json::from_str(raw.get()).ok()?;
                if slug.is_empty() {
                    std::borrow::Cow::Borrowed(inherited_model?)
                } else {
                    slug
                }
            }
            None => std::borrow::Cow::Borrowed(inherited_model?),
        };
        if !lock(&self.fast_models).contains(model.as_ref())
            || fields
                .get("service_tier")
                .is_some_and(|raw| raw.get() == "\"priority\"")
        {
            return None;
        }
        fields.insert(
            "service_tier".to_owned(),
            serde_json::from_str("\"priority\"").ok()?,
        );
        serde_json::to_vec(&fields).ok()
    }

    pub(super) fn reload(&self) -> ModelPolicy {
        let mut state = lock(&self.state);
        let (policy, reason) = ModelPolicy::load(&self.path, Some(&state.policy));
        if reason.is_none() {
            state.policy = policy;
        }
        state.reason = reason;
        state.policy.clone()
    }

    pub(super) fn status(&self) -> ModelPolicyStatus {
        let state = lock(&self.state);
        ModelPolicyStatus {
            default_transport: state.policy.default.as_str().to_owned(),
            models: state
                .policy
                .models
                .iter()
                .map(|(model, transport)| (model.clone(), transport.as_str().to_owned()))
                .collect(),
            reason: state.reason.clone(),
        }
    }

    pub(super) fn update(&self, update: ModelPolicyUpdate) -> Result<ModelPolicyStatus, String> {
        let source = serde_json::json!({
            "version": POLICY_VERSION,
            "default_transport": update.default_transport,
            "models": update.models.into_iter().map(|(model, transport)| {
                (model, serde_json::json!({"transport": transport}))
            }).collect::<serde_json::Map<_, _>>(),
        });
        let bytes = serde_json::to_vec_pretty(&source).map_err(|_| "serialize_failed")?;
        let policy =
            ModelPolicy::parse(std::str::from_utf8(&bytes).map_err(|_| "serialize_failed")?)
                .map_err(str::to_owned)?;
        write_atomic(&self.path, &bytes).map_err(|_| "write_failed")?;
        let mut state = lock(&self.state);
        state.policy = policy;
        state.reason = None;
        drop(state);
        Ok(self.status())
    }
}

fn is_fast_model(slug: &str) -> bool {
    slug.len() > 4
        && slug.trim() == slug
        && slug
            .get(..4)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("gpt-"))
}

impl Transport {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Http => "http",
        }
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("policy path has no parent"))?;
    fs::create_dir_all(parent)?;
    let mut temp = NamedTempFile::new_in(parent)?;
    temp.write_all(bytes)?;
    temp.flush()?;
    temp.persist(path).map_err(|error| error.error)?;
    Ok(())
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
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
    use super::{ModelPolicy, ModelPolicyStore, ModelPolicyUpdate, Transport};
    use std::fs;

    #[test]
    fn fast_preferences_survive_restart_and_a_failed_write_does_not_enable_fast()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("policy.json");
        let fast_path = path.with_file_name(super::FAST_MODELS_FILE);
        let store = ModelPolicyStore::new(path.clone());
        fs::create_dir(&fast_path)?;
        assert!(store.set_fast("gpt-6.2-sol".to_owned(), true).is_err());
        assert!(store.fast_models().is_empty());
        fs::remove_dir(fast_path)?;

        store.set_fast("gpt-6.2-sol".to_owned(), true)?;
        let restarted = ModelPolicyStore::new(path);
        assert_eq!(
            restarted.fast_models(),
            std::collections::BTreeSet::from(["gpt-6.2-sol".to_owned()])
        );
        assert!(restarted.set_fast("gemini-3".to_owned(), true).is_err());
        Ok(())
    }

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
    fn auto_policy_remains_auto_without_capability_override() {
        let policy = ModelPolicy::default();

        assert_eq!(
            policy.transport_for_payload(br#"{"model":"gpt"}"#),
            Transport::Auto
        );
    }

    #[test]
    fn missing_policy_uses_builtin_auto_without_failure() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("missing.json");
        let (loaded, reason) = ModelPolicy::load(&path, None);
        assert_eq!(loaded.transport_for(Some("any-model")), Transport::Auto);
        assert!(reason.is_none());
    }

    #[test]
    fn removed_policy_returns_builtin_auto_after_last_good_snapshot() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("removed.json");
        let previous = ModelPolicy::parse(
            r#"{"version":1,"default_transport":"http","models":{"gpt":{"transport":"http"}}}"#,
        )
        .expect("valid policy");
        let (loaded, reason) = ModelPolicy::load(&path, Some(&previous));
        assert_eq!(loaded.transport_for(Some("gpt")), Transport::Auto);
        assert_eq!(loaded.transport_for(None), Transport::Auto);
        assert!(reason.is_none());
    }

    #[test]
    fn reload_keeps_last_good_and_exposes_failure_reason() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("ai_cove_turbo_model_policy.json");
        fs::write(
            &path,
            r#"{"version":1,"default_transport":"http","models":{}}"#,
        )
        .expect("write policy");
        let store = ModelPolicyStore::new(path.clone());
        fs::write(path, "{bad").expect("break policy");
        assert_eq!(store.reload().transport_for(None), Transport::Http);
        assert_eq!(store.status().reason.as_deref(), Some("invalid_json"));
    }

    #[test]
    fn update_writes_hidden_policy_and_reloads_new_snapshot() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("ai_cove_turbo_model_policy.json");
        let store = ModelPolicyStore::new(path.clone());
        let status = store
            .update(ModelPolicyUpdate {
                default_transport: "auto".to_owned(),
                models: std::iter::once(("gpt-http".to_owned(), "http".to_owned())).collect(),
            })
            .expect("update policy");
        assert_eq!(
            status.models.get("gpt-http").map(String::as_str),
            Some("http")
        );
        assert!(
            fs::read_to_string(path)
                .expect("read policy")
                .contains("gpt-http")
        );
    }
}
