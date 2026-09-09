use std::{
    collections::HashMap,
    fs,
    io::Write,
    path::Path,
    sync::Mutex,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use axum::http::{HeaderMap, HeaderName, header};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;
use url::Url;

const CAPABILITY_TTL: Duration = Duration::from_secs(5 * 60);
const CAPABILITY_SNAPSHOT_VERSION: u64 = 1;
pub(super) const CAPABILITY_SNAPSHOT_FILE: &str = "transport-capabilities.json";

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CapabilityModelStatus {
    pub(crate) allowed: bool,
    pub(crate) transport: CapabilityTransport,
    pub(crate) reason_code: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub(crate) enum CapabilityTransport {
    #[serde(rename = "websocket")]
    WebSocket,
    #[serde(rename = "http")]
    HttpOnly,
}

impl CapabilityTransport {
    #[cfg(test)]
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::WebSocket => "websocket",
            Self::HttpOnly => "http",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct CapabilityResponse {
    success: bool,
    version: u64,
    object: String,
    data: Vec<CapabilityItem>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct CapabilityItem {
    model: String,
    allowed: bool,
    http: bool,
    responses_websocket: bool,
    reason_code: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CapabilitySnapshot {
    version: u64,
    fetched_at_unix_ms: u64,
    #[serde(default)]
    scope: String,
    response: CapabilityResponse,
}

impl CapabilityResponse {
    pub(super) fn parse(bytes: &[u8]) -> Result<Self, &'static str> {
        let response: Self = serde_json::from_slice(bytes).map_err(|_| "invalid_response")?;
        response.validate()?;
        Ok(response)
    }

    fn validate(&self) -> Result<(), &'static str> {
        if !self.success
            || self.version != CAPABILITY_SNAPSHOT_VERSION
            || self.object != "transport_capabilities"
        {
            return Err("unsupported_response");
        }
        Ok(())
    }
}

#[derive(Debug, Default)]
struct CacheState {
    expires_at: Option<Instant>,
    models: Vec<String>,
    reason: Option<String>,
    refreshing: bool,
    statuses: HashMap<String, CapabilityModelStatus>,
}

#[derive(Debug, Default)]
pub(super) struct CapabilityCache(Mutex<CacheState>);

impl CapabilityCache {
    #[cfg(test)]
    pub(super) fn load(path: &Path) -> Self {
        Self::load_for_scope(path, "")
    }

    pub(super) fn load_for_scope(path: &Path, scope: &str) -> Self {
        let cache = Self::default();
        let Ok(bytes) = fs::read(path) else {
            return cache;
        };
        let Ok(snapshot) = serde_json::from_slice::<CapabilitySnapshot>(&bytes) else {
            return cache;
        };
        if snapshot.version != CAPABILITY_SNAPSHOT_VERSION
            || snapshot.scope != scope
            || snapshot.response.validate().is_err()
        {
            return cache;
        }
        cache.apply(
            &snapshot.response,
            remaining_ttl(snapshot.fetched_at_unix_ms),
        );
        cache
    }

    pub(super) fn apply(&self, response: &CapabilityResponse, ttl: Duration) {
        let new_statuses = response.data.iter().map(|item| {
            let transport = if item.allowed && item.responses_websocket {
                CapabilityTransport::WebSocket
            } else {
                CapabilityTransport::HttpOnly
            };
            (
                item.model.clone(),
                CapabilityModelStatus {
                    allowed: item.allowed,
                    transport,
                    reason_code: item.reason_code.clone(),
                },
            )
        });
        let mut state = lock(&self.0);
        state.statuses = new_statuses.collect();
        let mut all_models = state.statuses.keys().cloned().collect::<Vec<_>>();
        all_models.sort_unstable();
        state.models = all_models;
        state.refreshing = false;
        state.expires_at = Some(Instant::now() + ttl);
        state.reason = None;
    }

    pub(super) fn needs_refresh(&self, models: &[String]) -> bool {
        let state = lock(&self.0);
        let expired = state
            .expires_at
            .is_none_or(|expires_at| Instant::now() >= expires_at);
        let needs_refresh = expired
            || models
                .iter()
                .any(|model| !state.statuses.contains_key(model));
        drop(state);
        needs_refresh
    }

    pub(super) fn needs_refresh_for(&self, model: &str) -> bool {
        let state = lock(&self.0);
        state
            .expires_at
            .is_none_or(|expires_at| Instant::now() >= expires_at)
            || !state.statuses.contains_key(model)
    }

    pub(super) fn statuses(&self) -> HashMap<String, CapabilityModelStatus> {
        lock(&self.0).statuses.clone()
    }

    pub(super) fn reason(&self) -> Option<String> {
        lock(&self.0).reason.clone()
    }

    #[cfg(test)]
    pub(super) fn transport_for(&self, model: &str) -> CapabilityTransport {
        lock(&self.0)
            .statuses
            .get(model)
            .map_or(CapabilityTransport::HttpOnly, |status| status.transport)
    }

    pub(super) fn known_transport_for(&self, model: &str) -> Option<CapabilityTransport> {
        let state = lock(&self.0);
        let expired = state
            .expires_at
            .is_none_or(|expires_at| Instant::now() >= expires_at);
        state.statuses.get(model).map(|status| {
            if expired && status.transport == CapabilityTransport::WebSocket {
                CapabilityTransport::HttpOnly
            } else {
                status.transport
            }
        })
    }

    pub(super) fn snapshot_response(&self) -> CapabilityResponse {
        let state = lock(&self.0);
        let mut data = state
            .statuses
            .iter()
            .map(|(model, status)| {
                let allowed = status.allowed;
                let responses_websocket = status.transport == CapabilityTransport::WebSocket;
                let http = status.transport == CapabilityTransport::HttpOnly || responses_websocket;
                CapabilityItem {
                    model: model.clone(),
                    allowed,
                    http,
                    responses_websocket,
                    reason_code: status.reason_code.clone(),
                }
            })
            .collect::<Vec<_>>();
        data.sort_unstable_by(|a, b| a.model.cmp(&b.model));
        drop(state);
        CapabilityResponse {
            success: true,
            version: CAPABILITY_SNAPSHOT_VERSION,
            object: "transport_capabilities".to_owned(),
            data,
        }
    }

    pub(super) fn begin_refresh(&self) -> bool {
        let mut state = lock(&self.0);
        if state.refreshing {
            return false;
        }
        state.refreshing = true;
        true
    }

    pub(super) fn mark_attempt(&self, _models: &[String], reason: Option<String>) {
        let mut state = lock(&self.0);
        state.refreshing = false;
        state.reason = reason;
        if state.reason.is_some() {
            state.expires_at = Some(Instant::now() + Duration::from_secs(5));
        }
    }

    #[cfg(test)]
    fn expire_for_test(&self) {
        lock(&self.0).expires_at = Some(Instant::now());
    }

    #[cfg(test)]
    pub(super) fn set_for_test(&self, model: &str, transport: CapabilityTransport) {
        let mut state = lock(&self.0);
        state.statuses.insert(
            model.to_owned(),
            CapabilityModelStatus {
                allowed: true,
                transport,
                reason_code: "test".to_owned(),
            },
        );
        state.expires_at = Some(Instant::now() + CAPABILITY_TTL);
    }
}

#[cfg(test)]
pub(super) fn persist_snapshot(path: &Path, response: &CapabilityResponse) -> std::io::Result<()> {
    persist_snapshot_for_scope(path, response, "")
}

pub(super) fn persist_snapshot_for_scope(
    path: &Path,
    response: &CapabilityResponse,
    scope: &str,
) -> std::io::Result<()> {
    let snapshot = CapabilitySnapshot {
        version: CAPABILITY_SNAPSHOT_VERSION,
        fetched_at_unix_ms: unix_time_ms(),
        scope: scope.to_owned(),
        response: response.clone(),
    };
    let bytes = serde_json::to_vec_pretty(&snapshot).map_err(std::io::Error::other)?;
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("capability snapshot path has no parent"))?;
    fs::create_dir_all(parent)?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    temporary.write_all(&bytes)?;
    temporary.flush()?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

pub(super) fn scope_key(upstream: &Url, headers: &HeaderMap) -> String {
    let mut hasher = Sha256::new();
    hasher.update(upstream.as_str().as_bytes());
    hasher.update([0]);
    for name in [
        header::AUTHORIZATION,
        HeaderName::from_static("x-ai-cove-client"),
        HeaderName::from_static("x-ai-cove-client-version"),
    ] {
        hasher.update(name.as_str().as_bytes());
        hasher.update(b"=");
        if let Some(value) = headers.get(&name) {
            hasher.update(value.as_bytes());
        }
        hasher.update([0]);
    }
    format!("{:x}", hasher.finalize())
}

pub(super) async fn fetch_batch(
    client: &reqwest::Client,
    upstream: &Url,
    headers: &HeaderMap,
    models: &[String],
) -> Result<CapabilityResponse, &'static str> {
    let mut target = upstream.clone();
    let base = upstream.path().trim_end_matches('/');
    let base = if base.is_empty() { "/v1" } else { base };
    target.set_path(&format!("{base}/transport/capabilities"));
    target
        .query_pairs_mut()
        .append_pair("models", &models.join(","));
    let response = client
        .get(target)
        .headers(capability_request_headers(headers))
        .send()
        .await
        .map_err(|_| "request_failed")?;
    if !response.status().is_success() {
        return Err(match response.status().as_u16() {
            401 => "request_rejected_401",
            403 => "request_rejected_403",
            404 => "request_rejected_404",
            500..=599 => "request_rejected_5xx",
            _ => "request_rejected",
        });
    }
    let bytes = response.bytes().await.map_err(|_| "response_failed")?;
    CapabilityResponse::parse(&bytes)
}

fn capability_request_headers(headers: &HeaderMap) -> HeaderMap {
    let mut sanitized = HeaderMap::new();
    for name in [
        header::AUTHORIZATION,
        HeaderName::from_static("x-ai-cove-client"),
        HeaderName::from_static("x-ai-cove-client-version"),
    ] {
        if let Some(value) = headers.get(&name) {
            sanitized.insert(name, value.clone());
        }
    }
    sanitized
}

pub(super) const fn ttl() -> Duration {
    CAPABILITY_TTL
}

fn remaining_ttl(fetched_at_unix_ms: u64) -> Duration {
    let age = unix_time_ms().saturating_sub(fetched_at_unix_ms);
    CAPABILITY_TTL.saturating_sub(Duration::from_millis(age))
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or(u64::MAX)
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[cfg(test)]
mod tests {
    use super::{CapabilityCache, CapabilityResponse, persist_snapshot};
    use axum::{
        Router,
        http::{HeaderMap, HeaderValue, header},
        routing::get,
    };
    use std::{error::Error, fs, time::Duration};
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn root_upstream_reaches_v1_capability_endpoint() -> Result<(), Box<dyn Error>> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let app = Router::new().route(
            "/v1/transport/capabilities",
            get(|| async {
                r#"{"success":true,"version":1,"object":"transport_capabilities","data":[{"model":"gpt-5.6-sol","allowed":true,"http":true,"responses_websocket":true,"reason_code":"ok"}]}"#
            }),
        );
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let response = super::fetch_batch(
            &reqwest::Client::new(),
            &url::Url::parse(&format!("http://{address}/"))?,
            &HeaderMap::new(),
            &["gpt-5.6-sol".to_owned()],
        )
        .await?;

        assert_eq!(response.data[0].model, "gpt-5.6-sol");
        server.abort();
        Ok(())
    }

    #[test]
    fn successful_apply_suppresses_refresh_until_expiry() {
        let cache = CapabilityCache::default();
        let response = CapabilityResponse::parse(
            br#"{"success":true,"version":1,"object":"transport_capabilities","data":[{"model":"gpt-http","allowed":true,"http":true,"responses_websocket":false,"reason_code":"no_responses_websocket_channel"}]}"#,
        )
        .expect("valid capability response");
        cache.apply(&response, Duration::from_secs(30));
        assert!(!cache.needs_refresh(&["gpt-http".to_owned()]));
    }

    #[test]
    fn capability_snapshot_uses_five_minute_refresh_window() {
        assert_eq!(super::ttl(), Duration::from_secs(5 * 60));
    }

    #[test]
    fn capability_refresh_is_single_flight() {
        let cache = CapabilityCache::default();

        assert!(cache.begin_refresh());
        assert!(!cache.begin_refresh());
        cache.mark_attempt(&["gpt-http".to_owned()], None);
        assert!(cache.begin_refresh());
    }

    #[test]
    fn expired_snapshot_keeps_last_status_for_display() {
        let cache = CapabilityCache::default();
        let response = CapabilityResponse::parse(
            br#"{"success":true,"version":1,"object":"transport_capabilities","data":[{"model":"gpt-http","allowed":true,"http":true,"responses_websocket":false,"reason_code":"no_responses_websocket_channel"}]}"#,
        )
        .expect("valid capability response");
        cache.apply(&response, Duration::from_secs(30));
        cache.expire_for_test();

        assert_eq!(
            cache
                .statuses()
                .get("gpt-http")
                .map(|status| status.transport.as_str()),
            Some("http")
        );
    }

    #[test]
    fn failed_refresh_keeps_last_status_for_display() {
        let cache = CapabilityCache::default();
        let response = CapabilityResponse::parse(
            br#"{"success":true,"version":1,"object":"transport_capabilities","data":[{"model":"gpt-http","allowed":true,"http":true,"responses_websocket":false,"reason_code":"no_responses_websocket_channel"}]}"#,
        )
        .expect("valid capability response");
        cache.apply(&response, Duration::from_secs(30));
        cache.expire_for_test();
        cache.mark_attempt(&["gpt-http".to_owned()], Some("request_failed".to_owned()));

        assert_eq!(
            cache
                .statuses()
                .get("gpt-http")
                .map(|status| status.transport.as_str()),
            Some("http")
        );
        assert_eq!(cache.reason().as_deref(), Some("request_failed"));
    }

    #[test]
    fn capability_headers_keep_startup_auth_and_drop_client_websocket_handshake() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer startup-key"),
        );
        headers.insert("x-ai-cove-client", HeaderValue::from_static("turbo"));
        headers.insert(
            "x-ai-cove-client-version",
            HeaderValue::from_static("mac/0.1.0-test"),
        );
        headers.insert(
            header::SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static("openai-insecure-api-key-stale-key"),
        );
        headers.insert(
            header::SEC_WEBSOCKET_KEY,
            HeaderValue::from_static("stale-handshake"),
        );
        headers.insert("session-id", HeaderValue::from_static("session-1"));

        let sanitized = super::capability_request_headers(&headers);

        assert_eq!(
            sanitized.get(header::AUTHORIZATION),
            Some(&HeaderValue::from_static("Bearer startup-key"))
        );
        assert_eq!(
            sanitized.get("x-ai-cove-client"),
            Some(&HeaderValue::from_static("turbo"))
        );
        assert_eq!(
            sanitized.get("x-ai-cove-client-version"),
            Some(&HeaderValue::from_static("mac/0.1.0-test"))
        );
        assert!(!sanitized.contains_key(header::SEC_WEBSOCKET_PROTOCOL));
        assert!(!sanitized.contains_key(header::SEC_WEBSOCKET_KEY));
        assert!(!sanitized.contains_key("session-id"));
    }

    #[test]
    fn disallowed_model_uses_http_transport_when_ws_is_not_usable() {
        let cache = CapabilityCache::default();
        let response = CapabilityResponse::parse(
            br#"{"success":true,"version":1,"object":"transport_capabilities","data":[{"model":"blocked","allowed":false,"http":true,"responses_websocket":true,"reason_code":"model_not_allowed"}]}"#,
        )
        .expect("valid capability response");
        cache.apply(&response, Duration::from_secs(30));
        assert_eq!(
            cache
                .statuses()
                .get("blocked")
                .map(|status| status.transport.as_str()),
            Some("http")
        );
        assert_eq!(
            cache.statuses().get("blocked").map(|status| status.allowed),
            Some(false)
        );
    }

    #[test]
    fn no_http_channel_still_uses_the_binary_http_transport_mode() {
        let cache = CapabilityCache::default();
        let response = CapabilityResponse::parse(
            br#"{"success":true,"version":1,"object":"transport_capabilities","data":[{"model":"unavailable","allowed":true,"http":false,"responses_websocket":false,"reason_code":"no_http_channel"}]}"#,
        )
        .expect("valid capability response");
        cache.apply(&response, Duration::from_secs(30));
        assert_eq!(
            cache
                .statuses()
                .get("unavailable")
                .map(|status| status.transport.as_str()),
            Some("http")
        );
    }

    #[test]
    fn missing_model_uses_conservative_http_transport_for_auto_routing() {
        let cache = CapabilityCache::default();

        assert_eq!(
            cache.transport_for("not-yet-probed"),
            super::CapabilityTransport::HttpOnly
        );
    }

    #[test]
    fn scoped_snapshot_does_not_cross_authentication_contexts() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("transport-capabilities.json");
        let response = CapabilityResponse::parse(
            br#"{"success":true,"version":1,"object":"transport_capabilities","data":[{"model":"gpt-ws","allowed":true,"http":true,"responses_websocket":true,"reason_code":"ok"}]}"#,
        )?;

        super::persist_snapshot_for_scope(&path, &response, "scope-a")?;

        assert!(
            super::CapabilityCache::load_for_scope(&path, "scope-a")
                .statuses()
                .contains_key("gpt-ws")
        );
        assert!(
            super::CapabilityCache::load_for_scope(&path, "scope-b")
                .statuses()
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn capability_scope_key_is_stable_without_persisting_raw_authorization() {
        let upstream = url::Url::parse("https://api.ai-cove.com/v1").expect("valid URL");
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer secret-key"),
        );

        let scope = super::scope_key(&upstream, &headers);

        assert_eq!(scope.len(), 64);
        assert!(!scope.contains("secret-key"));
    }

    #[test]
    fn websocket_capability_snapshot_also_advertises_http() -> Result<(), Box<dyn Error>> {
        let response = CapabilityResponse::parse(
            br#"{"success":true,"version":1,"object":"transport_capabilities","data":[{"model":"gpt-ws","allowed":true,"http":false,"responses_websocket":true,"reason_code":"ok"}]}"#,
        )?;
        let cache = CapabilityCache::default();
        cache.apply(&response, Duration::from_secs(30));

        let serialized = serde_json::to_value(cache.snapshot_response())?;
        let item = &serialized["data"][0];
        assert_eq!(item["http"], serde_json::Value::Bool(true));
        assert_eq!(item["responses_websocket"], serde_json::Value::Bool(true));
        Ok(())
    }

    #[test]
    fn persisted_snapshot_restores_status_after_restart() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("transport-capabilities.json");
        let response = CapabilityResponse::parse(
            br#"{"success":true,"version":1,"object":"transport_capabilities","data":[{"model":"gpt-http","allowed":true,"http":true,"responses_websocket":false,"reason_code":"no_responses_websocket_channel"}]}"#,
        )?;
        let cache = CapabilityCache::default();
        cache.apply(&response, Duration::from_secs(30));
        persist_snapshot(&path, &response)?;

        let restored = CapabilityCache::load(&path);
        assert_eq!(
            restored
                .statuses()
                .get("gpt-http")
                .map(|status| status.transport.as_str()),
            Some("http")
        );
        Ok(())
    }

    #[test]
    fn invalid_persisted_snapshot_falls_back_to_empty_cache() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("transport-capabilities.json");
        fs::write(&path, b"{bad")?;

        let cache = CapabilityCache::load(&path);

        assert!(cache.statuses().is_empty());
        Ok(())
    }
}
