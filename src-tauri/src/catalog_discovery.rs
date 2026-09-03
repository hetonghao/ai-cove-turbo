use std::{
    collections::HashSet,
    fmt,
    time::{SystemTime, UNIX_EPOCH},
};

use axum::http::{HeaderMap, HeaderValue, header};
use serde::Serialize;
use serde_json::Value;
use url::Url;

use crate::catalog::{self, CatalogModel};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DiscoveryResult {
    pub(crate) scope: &'static str,
    pub(crate) source_url: String,
    pub(crate) source_version: Option<String>,
    pub(crate) fetched_at: String,
    pub(crate) etag: Option<String>,
    pub(crate) models: Vec<CatalogModel>,
}

#[derive(Debug)]
pub(crate) enum DiscoveryError {
    MissingCredentials,
    InvalidUpstream,
    Unauthorized,
    Forbidden,
    NotFound,
    Timeout,
    Network,
    InvalidResponse,
    NoModels,
    MetadataWrite,
}

impl fmt::Display for DiscoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::MissingCredentials => "当前 Provider 没有可用于模型发现的 API Key",
            Self::InvalidUpstream => "当前上游地址不支持模型发现",
            Self::Unauthorized => "上游拒绝了当前 API Key（401）",
            Self::Forbidden => "当前 API Key 没有模型列表权限（403）",
            Self::NotFound => "上游没有提供可用的模型列表接口（404）",
            Self::Timeout => "模型发现请求超时",
            Self::Network => "模型发现请求未能连接到上游",
            Self::InvalidResponse => "上游模型列表格式不兼容",
            Self::NoModels => "当前 API Key 没有可用模型",
            Self::MetadataWrite => "无法保存模型目录来源元数据",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for DiscoveryError {}

pub(crate) async fn fetch(
    client: &reqwest::Client,
    upstream: &Url,
    headers: &HeaderMap,
    client_version: &str,
) -> Result<DiscoveryResult, DiscoveryError> {
    if !headers.contains_key(header::AUTHORIZATION) {
        return Err(DiscoveryError::MissingCredentials);
    }
    let mut endpoint = upstream.clone();
    let path = format!("{}/models", upstream.path().trim_end_matches('/'));
    endpoint.set_path(if path == "/models" {
        "/v1/models"
    } else {
        &path
    });
    endpoint
        .query_pairs_mut()
        .append_pair("client_version", client_version);
    if endpoint.scheme() != "https" {
        return Err(DiscoveryError::InvalidUpstream);
    }
    let response = client
        .get(endpoint.clone())
        .headers(safe_headers(headers))
        .timeout(std::time::Duration::from_secs(8))
        .send()
        .await
        .map_err(|error| {
            if error.is_timeout() {
                DiscoveryError::Timeout
            } else {
                DiscoveryError::Network
            }
        })?;
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(DiscoveryError::Unauthorized);
    }
    if status == reqwest::StatusCode::FORBIDDEN {
        return Err(DiscoveryError::Forbidden);
    }
    if status == reqwest::StatusCode::NOT_FOUND {
        return Err(DiscoveryError::NotFound);
    }
    if !status.is_success() {
        return Err(DiscoveryError::Network);
    }
    let etag = response
        .headers()
        .get(header::ETAG)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let body = response
        .bytes()
        .await
        .map_err(|_| DiscoveryError::Network)?;
    let document: Value =
        serde_json::from_slice(&body).map_err(|_| DiscoveryError::InvalidResponse)?;
    let (models, source_version) = parse_models(&document)?;
    if models.is_empty() {
        return Err(DiscoveryError::NoModels);
    }
    let mut source_url = endpoint;
    source_url.set_query(None);
    source_url.set_fragment(None);
    Ok(DiscoveryResult {
        scope: "current_key",
        source_url: source_url.to_string(),
        source_version,
        fetched_at: unix_time_ms().to_string(),
        etag,
        models,
    })
}

fn safe_headers(headers: &HeaderMap) -> HeaderMap {
    let mut safe = HeaderMap::new();
    for name in [header::AUTHORIZATION] {
        if let Some(value) = headers.get(&name) {
            safe.insert(name, value.clone());
        }
    }
    safe.insert("x-ai-cove-client", HeaderValue::from_static("turbo"));
    safe
}

fn parse_models(document: &Value) -> Result<(Vec<CatalogModel>, Option<String>), DiscoveryError> {
    let source_version = document
        .get("version")
        .and_then(Value::as_str)
        .map(str::to_owned);
    if let Some(values) = document.get("models").and_then(Value::as_array) {
        let mut seen = HashSet::new();
        let models = values
            .iter()
            .filter_map(|value| {
                let slug = value.get("slug")?.as_str()?.trim();
                if slug.is_empty() {
                    return None;
                }
                if !seen.insert(slug.to_owned()) {
                    return None;
                }
                let mut model = catalog::model_from_discovery(value, slug.to_owned());
                mark_field_sources(value, &mut model);
                Some(model.with_safe_defaults())
            })
            .collect();
        return Ok((models, source_version));
    }
    let Some(values) = document.get("data").and_then(Value::as_array) else {
        return Err(DiscoveryError::InvalidResponse);
    };
    let mut seen = HashSet::new();
    let models = values
        .iter()
        .filter_map(|value| value.get("id").and_then(Value::as_str))
        .map(str::trim)
        .filter(|slug| !slug.is_empty())
        .filter(|slug| seen.insert((*slug).to_owned()))
        .map(|slug| CatalogModel::basic(slug.to_owned()))
        .collect();
    Ok((models, None))
}

fn mark_field_sources(value: &Value, model: &mut CatalogModel) {
    for (json_field, catalog_field) in [
        ("slug", "slug"),
        ("display_name", "displayName"),
        ("description", "description"),
        ("context_window", "contextWindow"),
        ("max_context_window", "maxContextWindow"),
        ("supported_reasoning_levels", "supportedReasoningLevels"),
        ("default_reasoning_level", "defaultReasoningLevel"),
        (
            "supports_reasoning_summary_parameter",
            "supportsReasoningSummaryParameter",
        ),
        ("default_reasoning_summary", "defaultReasoningSummary"),
        ("input_modalities", "inputModalities"),
        ("service_tiers", "serviceTiers"),
        ("default_service_tier", "defaultServiceTier"),
        ("use_responses_lite", "useResponsesLite"),
        ("prefer_websockets", "preferWebsockets"),
        (
            "supports_image_detail_original",
            "supportsImageDetailOriginal",
        ),
        ("supports_search_tool", "supportsSearchTool"),
        ("supports_parallel_tool_calls", "supportsParallelToolCalls"),
        ("tool_mode", "toolMode"),
        ("experimental_supported_tools", "experimentalSupportedTools"),
        ("base_instructions", "baseInstructions"),
        ("minimal_client_version", "minimalClientVersion"),
    ] {
        model.field_sources.insert(
            catalog_field.to_owned(),
            if value.get(json_field).is_some() {
                "上游"
            } else {
                "待确认"
            }
            .to_owned(),
        );
    }
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderMap, HeaderValue, header};

    use super::{parse_models, safe_headers};

    #[test]
    fn basic_data_models_do_not_claim_unknown_capabilities() {
        let (models, _) =
            parse_models(&serde_json::json!({"data":[{"id":"gpt-basic"},{"id":"gpt-basic"}]}))
                .expect("model list");
        assert_eq!(models.len(), 1);
        assert_eq!(
            models.first().map(|model| model.slug.as_str()),
            Some("gpt-basic")
        );
        assert!(
            models
                .first()
                .is_some_and(|model| model.supported_reasoning_levels.is_empty())
        );
        assert!(
            models
                .first()
                .is_some_and(|model| !model.supports_search_tool)
        );
    }

    #[test]
    fn codex_models_preserve_custom_reasoning_and_context() {
        let (models, _) = parse_models(&serde_json::json!({"version":"v1","models":[{
            "slug":"gpt-full","context_window":125_000,"max_context_window":250_000,
            "supported_reasoning_levels":[{"effort":"custom","description":"x"}],
            "default_reasoning_level":"custom","supports_reasoning_summary_parameter":true
        }]}))
        .expect("codex model list");
        assert_eq!(
            models.first().and_then(|model| model.context_window),
            Some(125_000)
        );
        assert_eq!(
            models
                .first()
                .and_then(|model| model.supported_reasoning_levels.first())
                .map(|level| level.effort.as_str()),
            Some("custom")
        );
        assert_eq!(
            models
                .first()
                .and_then(|model| model.field_sources.get("contextWindow")),
            Some(&"上游".to_owned())
        );
    }

    #[test]
    fn codex_models_fill_safe_defaults_for_missing_optional_fields() {
        let (models, _) = parse_models(&serde_json::json!({"models":[{
            "slug":"gpt-safe","context_window":125_000,"max_context_window":250_000,
            "supported_reasoning_levels":[{"effort":"low"}],"default_reasoning_level":"low"
        }]}))
        .expect("codex model list");
        let model = models.first().expect("model");
        assert_eq!(model.input_modalities, vec!["text", "image"]);
        assert_eq!(model.default_reasoning_summary.as_deref(), Some("none"));
        assert_eq!(
            model.field_sources.get("defaultReasoningSummary"),
            Some(&"模板".to_owned())
        );
        assert_eq!(
            model.field_sources.get("description"),
            Some(&"模板".to_owned())
        );
        assert_eq!(
            model.field_sources.get("inputModalities"),
            Some(&"模板".to_owned())
        );
        assert_eq!(
            model.field_sources.get("baseInstructions"),
            Some(&"模板".to_owned())
        );
    }

    #[test]
    fn discovery_headers_drop_cookies_and_unrelated_credentials() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer secret"),
        );
        headers.insert(header::COOKIE, HeaderValue::from_static("session=secret"));
        headers.insert("x-api-key", HeaderValue::from_static("secret"));
        let safe = safe_headers(&headers);
        assert_eq!(
            safe.get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok()),
            Some("Bearer secret")
        );
        assert!(safe.get(header::COOKIE).is_none());
        assert!(safe.get("x-api-key").is_none());
    }
}
