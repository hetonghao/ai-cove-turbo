use std::{collections::BTreeMap, fmt, path::PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

const DEFAULT_BASE_INSTRUCTIONS: &str = "You are Codex, an expert coding agent. Follow the user's instructions and work carefully in the current repository.";
const DEFAULT_SHELL_TYPE: &str = "shell_command";

fn default_shell_type() -> String {
    DEFAULT_SHELL_TYPE.to_owned()
}

const fn default_support_verbosity() -> bool {
    true
}

fn default_truncation_policy() -> Value {
    serde_json::json!({"mode": "tokens", "limit": 10_000})
}

// CLIPPY-ALLOW: Codex 的模型能力协议使用独立布尔字段，合并会改变 JSON 契约。
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CatalogModel {
    pub(crate) slug: String,
    pub(crate) display_name: String,
    pub(crate) description: String,
    pub(crate) visibility: String,
    pub(crate) priority: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) context_window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) max_context_window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) effective_context_window_percent: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) auto_compact_token_limit: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) truncation_policy: Option<Value>,
    #[serde(default = "default_shell_type")]
    pub(crate) shell_type: String,
    #[serde(default = "default_support_verbosity")]
    pub(crate) support_verbosity: bool,
    #[serde(default)]
    pub(crate) input_modalities: Vec<String>,
    #[serde(default)]
    pub(crate) supported_reasoning_levels: Vec<ReasoningLevel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) default_reasoning_level: Option<String>,
    #[serde(default)]
    pub(crate) supports_reasoning_summary_parameter: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) default_reasoning_summary: Option<String>,
    #[serde(default)]
    pub(crate) service_tiers: Vec<ServiceTier>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) default_service_tier: Option<String>,
    #[serde(default)]
    pub(crate) use_responses_lite: bool,
    #[serde(default)]
    pub(crate) prefer_websockets: bool,
    #[serde(default)]
    pub(crate) supports_image_detail_original: bool,
    #[serde(default)]
    pub(crate) supports_search_tool: bool,
    #[serde(default)]
    pub(crate) supports_parallel_tool_calls: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) tool_mode: Option<String>,
    #[serde(default)]
    pub(crate) experimental_supported_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) base_instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) minimal_client_version: Option<String>,
    #[serde(default)]
    pub(crate) supported_in_api: bool,
    #[serde(default)]
    pub(crate) root_presence: bool,
    #[serde(default)]
    pub(crate) root_missing: bool,
    #[serde(default)]
    pub(crate) field_sources: BTreeMap<String, String>,
    #[serde(default)]
    pub(crate) conflicts: Vec<String>,
}

impl Default for CatalogModel {
    fn default() -> Self {
        Self::basic(String::new())
    }
}

impl CatalogModel {
    pub(crate) fn is_gpt_like_slug(slug: &str) -> bool {
        let lower = slug.to_ascii_lowercase();
        lower.starts_with("gpt-")
            || lower.starts_with("o1")
            || lower.starts_with("o3")
            || lower.starts_with("o4")
            || lower.starts_with("ox-")
            || lower.starts_with("codex-")
            || lower == "chatgpt-4o-latest"
    }

    pub(crate) fn with_safe_defaults(mut self) -> Self {
        if self.description.trim().is_empty() {
            self.description = format!("Codex model {}", self.slug);
            self.field_sources
                .insert("description".to_owned(), "模板".to_owned());
        }
        if self.input_modalities.is_empty() {
            self.input_modalities = vec!["text".to_owned()];
            self.field_sources
                .insert("inputModalities".to_owned(), "模板".to_owned());
        }
        if self.truncation_policy.is_none()
            || self
                .truncation_policy
                .as_ref()
                .is_some_and(|policy| policy.as_str() == Some("auto"))
        {
            self.truncation_policy = Some(default_truncation_policy());
            self.field_sources
                .insert("truncationPolicy".to_owned(), "模板".to_owned());
        }
        if self.shell_type.trim().is_empty() {
            self.shell_type = default_shell_type();
            self.field_sources
                .insert("shellType".to_owned(), "模板".to_owned());
        }
        if self.auto_compact_token_limit.is_none() {
            self.auto_compact_token_limit = self
                .context_window
                .map(|context| context.saturating_mul(90) / 100);
            self.field_sources
                .insert("autoCompactTokenLimit".to_owned(), "模板".to_owned());
        }
        if self.base_instructions.as_deref().is_none_or(str::is_empty) {
            self.base_instructions = Some(DEFAULT_BASE_INSTRUCTIONS.to_owned());
            self.field_sources
                .insert("baseInstructions".to_owned(), "模板".to_owned());
        }
        if self
            .minimal_client_version
            .as_deref()
            .is_none_or(str::is_empty)
        {
            self.minimal_client_version = Some("0.0.0".to_owned());
            self.field_sources
                .insert("minimalClientVersion".to_owned(), "模板".to_owned());
        }
        if !Self::is_gpt_like_slug(&self.slug) {
            self.supports_reasoning_summary_parameter = false;
            self.default_reasoning_summary = Some("none".to_owned());
            self.service_tiers = Vec::new();
            self.default_service_tier = None;
            self.field_sources.insert(
                "supportsReasoningSummaryParameter".to_owned(),
                "模板".to_owned(),
            );
            self.field_sources
                .insert("defaultReasoningSummary".to_owned(), "模板".to_owned());
            self.field_sources
                .insert("serviceTiers".to_owned(), "模板".to_owned());
            self.field_sources
                .insert("defaultServiceTier".to_owned(), "模板".to_owned());
        } else if !self.supports_reasoning_summary_parameter {
            if self.default_reasoning_summary.as_deref() != Some("none") {
                self.default_reasoning_summary = Some("none".to_owned());
                self.field_sources
                    .insert("defaultReasoningSummary".to_owned(), "模板".to_owned());
            } else if !self.field_sources.contains_key("defaultReasoningSummary") {
                self.field_sources
                    .insert("defaultReasoningSummary".to_owned(), "模板".to_owned());
            }
        }
        self
    }

    pub(crate) fn validate_complete(&self) -> Result<(), String> {
        if self.slug.trim().is_empty() {
            return Err("模型缺少 slug".to_owned());
        }
        if self.display_name.trim().is_empty() {
            return Err(format!("模型 {} 缺少 display_name", self.slug));
        }
        if !matches!(self.visibility.as_str(), "list" | "hide" | "none") {
            return Err(format!("模型 {} 的 visibility 无效", self.slug));
        }
        if !self
            .truncation_policy
            .as_ref()
            .is_some_and(Value::is_object)
        {
            return Err(format!("模型 {} 缺少有效 truncation_policy", self.slug));
        }
        if self.shell_type.trim().is_empty() {
            return Err(format!("模型 {} 缺少 shell_type", self.slug));
        }
        let Some(maximum) = self.max_context_window else {
            return Err(format!("模型 {} 缺少最大上下文窗口", self.slug));
        };
        if maximum < 125_000 {
            return Err(format!("模型 {} 的最大上下文必须至少为 125000", self.slug));
        }
        let Some(context) = self.context_window else {
            return Err(format!("模型 {} 缺少当前上下文窗口", self.slug));
        };
        if !(125_000..=maximum).contains(&context) {
            return Err(format!("模型 {} 的当前上下文超出允许范围", self.slug));
        }
        if let Some(limit) = self.auto_compact_token_limit
            && limit > context.saturating_mul(90) / 100
        {
            return Err(format!(
                "模型 {} 的自动压缩阈值必须不超过上下文的 90%",
                self.slug
            ));
        }
        if let Some(percent) = self.effective_context_window_percent
            && !(1..=100).contains(&percent)
        {
            return Err(format!(
                "模型 {} 的有效上下文比例必须处于 1 至 100 之间",
                self.slug
            ));
        }
        if self.supported_reasoning_levels.is_empty() {
            return Err(format!(
                "模型 {} 缺少 supported reasoning effort",
                self.slug
            ));
        }
        let Some(default) = &self.default_reasoning_level else {
            return Err(format!("模型 {} 缺少默认 reasoning effort", self.slug));
        };
        if !self
            .supported_reasoning_levels
            .iter()
            .any(|level| level.effort == *default)
        {
            return Err(format!(
                "模型 {} 的默认 reasoning effort 不在支持列表中",
                self.slug
            ));
        }
        if let Some(default_tier) = &self.default_service_tier
            && !self
                .service_tiers
                .iter()
                .any(|tier| tier.id == *default_tier)
        {
            return Err(format!("模型 {} 的默认服务层不在支持列表中", self.slug));
        }
        if !self.supports_reasoning_summary_parameter
            && self
                .default_reasoning_summary
                .as_deref()
                .is_some_and(|value| value != "none")
        {
            return Err(format!("模型 {} 不支持 reasoning summary", self.slug));
        }
        Ok(())
    }

    pub(crate) fn basic(slug: String) -> Self {
        let mut field_sources = BTreeMap::new();
        field_sources.insert("slug".to_owned(), "上游".to_owned());
        field_sources.insert("displayName".to_owned(), "上游".to_owned());
        field_sources.insert("contextWindow".to_owned(), "待确认".to_owned());
        field_sources.insert("maxContextWindow".to_owned(), "待确认".to_owned());
        field_sources.insert("supportedReasoningLevels".to_owned(), "待确认".to_owned());
        Self {
            display_name: slug.clone(),
            description: "来自当前密钥可用模型列表".to_owned(),
            visibility: "list".to_owned(),
            priority: 0,
            context_window: None,
            max_context_window: None,
            effective_context_window_percent: None,
            auto_compact_token_limit: None,
            truncation_policy: Some(default_truncation_policy()),
            shell_type: default_shell_type(),
            support_verbosity: true,
            input_modalities: vec!["text".to_owned()],
            supported_reasoning_levels: Vec::new(),
            default_reasoning_level: None,
            supports_reasoning_summary_parameter: false,
            default_reasoning_summary: Some("none".to_owned()),
            service_tiers: Vec::new(),
            default_service_tier: None,
            use_responses_lite: false,
            prefer_websockets: false,
            supports_image_detail_original: false,
            supports_search_tool: false,
            supports_parallel_tool_calls: false,
            tool_mode: None,
            experimental_supported_tools: Vec::new(),
            base_instructions: None,
            minimal_client_version: None,
            supported_in_api: true,
            root_presence: false,
            root_missing: true,
            field_sources,
            conflicts: Vec::new(),
            slug,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReasoningLevel {
    pub(crate) effort: String,
    #[serde(default)]
    pub(crate) description: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ServiceTier {
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) description: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CatalogChange {
    pub(crate) slug: String,
    pub(crate) field: String,
    pub(crate) before: Option<String>,
    pub(crate) after: Option<String>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CatalogStatus {
    pub(crate) path: String,
    pub(crate) state: String,
    pub(crate) source_path: Option<String>,
    pub(crate) models: Vec<CatalogModel>,
    pub(crate) changes: Vec<CatalogChange>,
    pub(crate) restart_required: bool,
    pub(crate) loaded: bool,
    pub(crate) request_verified: bool,
    pub(crate) revision: String,
    #[serde(default)]
    pub(crate) metadata: CatalogMetadata,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CatalogMetadata {
    pub(crate) source_url: Option<String>,
    pub(crate) source_version: Option<String>,
    pub(crate) fetched_at: Option<String>,
    pub(crate) etag: Option<String>,
    pub(crate) metadata_path: Option<String>,
    #[serde(default)]
    pub(crate) field_sources: BTreeMap<String, BTreeMap<String, String>>,
    #[serde(default)]
    pub(crate) conflicts: BTreeMap<String, Vec<String>>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CatalogModelUpdate {
    pub(crate) slug: String,
    pub(crate) display_name: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) visibility: Option<String>,
    pub(crate) priority: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct OwnershipRecord {
    pub(crate) config_path: PathBuf,
    pub(crate) fixed_path: PathBuf,
    pub(crate) original_model_catalog_json: Option<String>,
    pub(crate) source_path: Option<PathBuf>,
    pub(crate) baseline_models: Vec<CatalogModel>,
    #[serde(default)]
    pub(crate) root_slugs: Vec<String>,
    #[serde(default)]
    pub(crate) baseline_document: Value,
}

#[derive(Debug)]
pub(crate) enum CatalogError {
    Read(std::io::Error),
    Write(std::io::Error),
    Json(serde_json::Error),
    Toml(toml_edit::TomlError),
    InvalidSchema(String),
    SourceUnavailable,
    OwnershipConflict,
    InvalidModel(String),
    ProtectedModel(String),
    ContentChanged,
}

impl fmt::Display for CatalogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read(error) => write!(formatter, "无法读取模型目录：{error}"),
            Self::Write(error) => write!(formatter, "无法写入模型目录：{error}"),
            Self::Json(error) => write!(formatter, "模型目录 JSON 无效：{error}"),
            Self::Toml(error) => write!(formatter, "Codex 配置 TOML 无法解析：{error}"),
            Self::InvalidSchema(message) => write!(formatter, "模型目录结构无效：{message}"),
            Self::SourceUnavailable => write!(formatter, "没有可验证的 Codex 模型目录来源"),
            Self::OwnershipConflict => {
                write!(formatter, "Codex 的 model_catalog_json 已被外部修改")
            }
            Self::InvalidModel(slug) => write!(formatter, "模型目录更新包含未知模型：{slug}"),
            Self::ProtectedModel(slug) => write!(formatter, "模型 {} 受保护，不能删除", slug),
            Self::ContentChanged => write!(formatter, "模型目录已被外部修改，请重新加载后保存"),
        }
    }
}

impl std::error::Error for CatalogError {}
