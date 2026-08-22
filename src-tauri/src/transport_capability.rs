use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Duration, Instant},
};

use axum::http::HeaderMap;
use serde::Deserialize;
use url::Url;

const CAPABILITY_TTL: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug)]
pub(super) struct CapabilityHint {
    pub(super) http_available: bool,
    pub(super) responses_websocket_available: bool,
}

#[derive(Debug, Deserialize)]
pub(super) struct CapabilityResponse {
    success: bool,
    version: u64,
    object: String,
    data: Vec<CapabilityItem>,
}

#[derive(Debug, Deserialize)]
struct CapabilityItem {
    model: String,
    allowed: bool,
    http: bool,
    responses_websocket: bool,
}

impl CapabilityResponse {
    pub(super) fn parse(bytes: &[u8]) -> Result<Self, &'static str> {
        let response: Self = serde_json::from_slice(bytes).map_err(|_| "invalid_response")?;
        if !response.success || response.version != 1 || response.object != "transport_capabilities"
        {
            return Err("unsupported_response");
        }
        Ok(response)
    }
}

#[derive(Debug, Default)]
struct CacheState {
    expires_at: Option<Instant>,
    hints: HashMap<String, CapabilityHint>,
    models: Vec<String>,
    reason: Option<String>,
}

#[derive(Debug, Default)]
pub(super) struct CapabilityCache(Mutex<CacheState>);

impl CapabilityCache {
    pub(super) fn apply(&self, response: CapabilityResponse, ttl: Duration) {
        let mut models = response
            .data
            .iter()
            .map(|item| item.model.clone())
            .collect::<Vec<_>>();
        models.sort_unstable();
        let hints = response
            .data
            .into_iter()
            .filter(|item| item.allowed)
            .map(|item| {
                (
                    item.model,
                    CapabilityHint {
                        http_available: item.http,
                        responses_websocket_available: item.responses_websocket,
                    },
                )
            })
            .collect();
        let mut state = lock(&self.0);
        state.hints = hints;
        state.models = models;
        state.expires_at = Some(Instant::now() + ttl);
        state.reason = None;
    }

    pub(super) fn hint(&self, model: &str) -> Option<CapabilityHint> {
        let state = lock(&self.0);
        if state
            .expires_at
            .is_none_or(|expires_at| Instant::now() >= expires_at)
        {
            return None;
        }
        state.hints.get(model).copied()
    }

    pub(super) fn needs_refresh(&self, models: &[String]) -> bool {
        let state = lock(&self.0);
        state.models != models
            || state
                .expires_at
                .is_none_or(|expires_at| Instant::now() >= expires_at)
    }

    pub(super) fn mark_attempt(&self, models: &[String], reason: Option<String>) {
        let mut state = lock(&self.0);
        state.models = models.to_vec();
        state.reason = reason;
        if state.reason.is_some() {
            state.hints.clear();
            state.expires_at = Some(Instant::now() + Duration::from_secs(5));
        }
    }

    #[cfg(test)]
    fn expire_for_test(&self) {
        lock(&self.0).expires_at = Some(Instant::now());
    }
}

pub(super) async fn fetch_batch(
    client: &reqwest::Client,
    upstream: &Url,
    headers: &HeaderMap,
    models: &[String],
) -> Result<CapabilityResponse, &'static str> {
    let mut target = upstream.clone();
    let base = upstream.path().trim_end_matches('/');
    target.set_path(&format!("{base}/transport/capabilities"));
    target
        .query_pairs_mut()
        .append_pair("models", &models.join(","));
    let response = client
        .get(target)
        .headers(headers.clone())
        .send()
        .await
        .map_err(|_| "request_failed")?;
    if !response.status().is_success() {
        return Err("request_rejected");
    }
    let bytes = response.bytes().await.map_err(|_| "response_failed")?;
    CapabilityResponse::parse(&bytes)
}

pub(super) const fn ttl() -> Duration {
    CAPABILITY_TTL
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[cfg(test)]
mod tests {
    use super::{CapabilityCache, CapabilityResponse};
    use std::time::Duration;

    #[test]
    fn batch_snapshot_expires_back_to_unknown_without_networking() {
        let cache = CapabilityCache::default();
        cache.apply(
            CapabilityResponse::parse(
                br#"{"success":true,"version":1,"object":"transport_capabilities","data":[{"model":"gpt-http","allowed":true,"http":true,"responses_websocket":false,"reason_code":"no_responses_websocket_channel"}]}"#,
            )
            .expect("valid capability response"),
            std::time::Duration::from_secs(30),
        );
        assert!(cache.hint("gpt-http").is_some());
        cache.expire_for_test();
        assert!(cache.hint("gpt-http").is_none());
    }

    #[test]
    fn successful_apply_suppresses_refresh_until_expiry() {
        let cache = CapabilityCache::default();
        let response = CapabilityResponse::parse(
            br#"{"success":true,"version":1,"object":"transport_capabilities","data":[{"model":"gpt-http","allowed":true,"http":true,"responses_websocket":false,"reason_code":"no_responses_websocket_channel"}]}"#,
        )
        .expect("valid capability response");
        cache.apply(response, Duration::from_secs(30));
        assert!(!cache.needs_refresh(&["gpt-http".to_owned()]));
    }
}
