use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use serde_json::{Value, json};
use tempfile::NamedTempFile;
use toml_edit::{DocumentMut, Item, value};

use super::catalog_diff::{diff_models, models_document};
use super::catalog_types::OwnershipRecord;
use super::{
    CatalogError, CatalogMetadata, CatalogModel, CatalogStatus, ReasoningLevel, ServiceTier,
};

pub(super) fn status_from_file(
    fixed_path: &Path,
    record: &OwnershipRecord,
    restart_required: bool,
    loaded: bool,
    request_verified: bool,
) -> Result<CatalogStatus, CatalogError> {
    let bytes = fs::read(fixed_path).map_err(CatalogError::Read)?;
    let document: Value = serde_json::from_slice(&bytes).map_err(CatalogError::Json)?;
    let metadata = read_metadata(fixed_path);
    let models = parse_models(&bytes)?
        .into_iter()
        .map(|mut model| {
            if let Some(sources) = metadata.field_sources.get(&model.slug) {
                model.field_sources.clone_from(sources);
            }
            if let Some(conflicts) = metadata.conflicts.get(&model.slug) {
                model.conflicts.clone_from(conflicts);
            }
            model
        })
        .collect();
    let baseline = if record.baseline_document.is_null() {
        models_document(&record.baseline_models)
    } else {
        record.baseline_document.clone()
    };
    Ok(CatalogStatus {
        path: fixed_path.display().to_string(),
        state: "owned".to_owned(),
        source_path: record
            .source_path
            .as_ref()
            .map(|path| path.display().to_string()),
        models,
        changes: diff_models(&baseline, &document),
        restart_required,
        loaded,
        request_verified,
        revision: digest(&bytes),
        metadata,
    })
}

pub(super) fn parse_models(bytes: &[u8]) -> Result<Vec<CatalogModel>, CatalogError> {
    let document: Value = serde_json::from_slice(bytes).map_err(CatalogError::Json)?;
    let models = document
        .get("models")
        .and_then(Value::as_array)
        .ok_or_else(|| CatalogError::InvalidSchema("缺少 models 数组".to_owned()))?;
    let mut result = Vec::with_capacity(models.len());
    let mut seen = std::collections::HashSet::new();
    for model in models {
        let slug = model
            .get("slug")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| CatalogError::InvalidSchema("模型缺少 slug".to_owned()))?
            .to_owned();
        if !seen.insert(slug.clone()) {
            return Err(CatalogError::InvalidSchema(format!(
                "模型 slug 重复：{slug}"
            )));
        }
        result.push(model_from_value(model, slug));
    }
    Ok(result)
}

pub(super) fn model_from_value(model: &Value, slug: String) -> CatalogModel {
    CatalogModel {
        display_name: string_field(model, "display_name"),
        description: string_field(model, "description"),
        visibility: string_field_or(model, "visibility", "list"),
        priority: model
            .get("priority")
            .and_then(Value::as_i64)
            .unwrap_or_default(),
        context_window: number_field(model, "context_window"),
        max_context_window: number_field(model, "max_context_window"),
        effective_context_window_percent: model
            .get("effective_context_window_percent")
            .and_then(Value::as_u64)
            .and_then(|value| u8::try_from(value).ok()),
        auto_compact_token_limit: number_field(model, "auto_compact_token_limit"),
        truncation_policy: model.get("truncation_policy").cloned(),
        shell_type: string_field_or(model, "shell_type", "shell_command"),
        support_verbosity: bool_field_or(model, "support_verbosity", true),
        input_modalities: string_array(model, "input_modalities"),
        supported_reasoning_levels: reasoning_levels(model),
        default_reasoning_level: optional_string(model, "default_reasoning_level"),
        supports_reasoning_summary_parameter: bool_field(
            model,
            "supports_reasoning_summary_parameter",
        ),
        default_reasoning_summary: optional_string(model, "default_reasoning_summary"),
        service_tiers: service_tiers(model),
        default_service_tier: optional_string(model, "default_service_tier"),
        use_responses_lite: bool_field(model, "use_responses_lite"),
        prefer_websockets: bool_field(model, "prefer_websockets"),
        supports_image_detail_original: bool_field(model, "supports_image_detail_original"),
        supports_search_tool: bool_field(model, "supports_search_tool"),
        supports_parallel_tool_calls: bool_field(model, "supports_parallel_tool_calls"),
        tool_mode: optional_string(model, "tool_mode"),
        experimental_supported_tools: string_array(model, "experimental_supported_tools"),
        base_instructions: optional_string(model, "base_instructions"),
        minimal_client_version: optional_string(model, "minimal_client_version"),
        supported_in_api: model
            .get("supported_in_api")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        field_sources: BTreeMap::default(),
        conflicts: Vec::new(),
        slug,
    }
}

fn number_field(model: &Value, key: &str) -> Option<u64> {
    model
        .get(key)
        .and_then(Value::as_u64)
        .filter(|value| *value > 0)
}

fn optional_string(model: &Value, key: &str) -> Option<String> {
    model.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn bool_field(model: &Value, key: &str) -> bool {
    model.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn bool_field_or(model: &Value, key: &str, fallback: bool) -> bool {
    model.get(key).and_then(Value::as_bool).unwrap_or(fallback)
}

fn string_array(model: &Value, key: &str) -> Vec<String> {
    model
        .get(key)
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn reasoning_levels(model: &Value) -> Vec<ReasoningLevel> {
    model
        .get("supported_reasoning_levels")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|value| {
                    value
                        .as_str()
                        .map(|effort| ReasoningLevel {
                            effort: effort.to_owned(),
                            description: String::new(),
                        })
                        .or_else(|| {
                            Some(ReasoningLevel {
                                effort: value.get("effort")?.as_str()?.to_owned(),
                                description: value
                                    .get("description")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_owned(),
                            })
                        })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn service_tiers(model: &Value) -> Vec<ServiceTier> {
    model
        .get("service_tiers")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|value| {
                    Some(ServiceTier {
                        id: value.get("id")?.as_str()?.to_owned(),
                        name: value
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        description: value
                            .get("description")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn metadata_path(fixed_path: &Path) -> PathBuf {
    fixed_path.with_file_name("ai_cove_turbo.metadata.json")
}

pub(super) fn read_metadata(fixed_path: &Path) -> CatalogMetadata {
    let path = metadata_path(fixed_path);
    let Ok(bytes) = fs::read(&path) else {
        return CatalogMetadata::default();
    };
    serde_json::from_slice::<CatalogMetadata>(&bytes).unwrap_or_default()
}

pub(super) fn write_metadata(
    fixed_path: &Path,
    metadata: &CatalogMetadata,
) -> Result<(), CatalogError> {
    let path = metadata_path(fixed_path);
    write_atomic(
        &path,
        &serde_json::to_vec_pretty(metadata).map_err(CatalogError::Json)?,
    )
}

fn string_field(model: &Value, key: &str) -> String {
    model
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn string_field_or(model: &Value, key: &str, fallback: &str) -> String {
    model
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or(fallback)
        .to_owned()
}

pub(super) fn set_string(model: &mut Value, key: &str, value: &str) {
    if let Some(object) = model.as_object_mut() {
        object.insert(key.to_owned(), Value::String(value.to_owned()));
    }
}

pub(super) fn set_number(model: &mut Value, key: &str, value: i64) {
    if let Some(object) = model.as_object_mut() {
        object.insert(key.to_owned(), serde_json::json!(value));
    }
}

pub(super) fn write_model_fields(target: &mut Value, model: &CatalogModel) {
    let Some(object) = target.as_object_mut() else {
        return;
    };
    object.insert("slug".to_owned(), Value::String(model.slug.clone()));
    object.insert(
        "display_name".to_owned(),
        Value::String(model.display_name.clone()),
    );
    object.insert(
        "description".to_owned(),
        Value::String(model.description.clone()),
    );
    object.insert(
        "visibility".to_owned(),
        Value::String(model.visibility.clone()),
    );
    object.insert("priority".to_owned(), json!(model.priority));
    insert_optional(object, "context_window", model.context_window);
    insert_optional(object, "max_context_window", model.max_context_window);
    insert_optional(
        object,
        "effective_context_window_percent",
        model.effective_context_window_percent,
    );
    insert_optional(
        object,
        "auto_compact_token_limit",
        model.auto_compact_token_limit,
    );
    if let Some(policy) = &model.truncation_policy {
        object.insert("truncation_policy".to_owned(), policy.clone());
    }
    object.insert("shell_type".to_owned(), json!(model.shell_type));
    object.insert(
        "support_verbosity".to_owned(),
        json!(model.support_verbosity),
    );
    object.insert("input_modalities".to_owned(), json!(model.input_modalities));
    object.insert(
        "supported_reasoning_levels".to_owned(),
        json!(model.supported_reasoning_levels),
    );
    insert_optional_string(
        object,
        "default_reasoning_level",
        model.default_reasoning_level.as_deref(),
    );
    object.insert(
        "supports_reasoning_summary_parameter".to_owned(),
        json!(model.supports_reasoning_summary_parameter),
    );
    insert_optional_string(
        object,
        "default_reasoning_summary",
        model.default_reasoning_summary.as_deref(),
    );
    object.insert("service_tiers".to_owned(), json!(model.service_tiers));
    insert_optional_string(
        object,
        "default_service_tier",
        model.default_service_tier.as_deref(),
    );
    object.insert(
        "use_responses_lite".to_owned(),
        json!(model.use_responses_lite),
    );
    object.insert(
        "prefer_websockets".to_owned(),
        json!(model.prefer_websockets),
    );
    object.insert(
        "supports_image_detail_original".to_owned(),
        json!(model.supports_image_detail_original),
    );
    object.insert(
        "supports_search_tool".to_owned(),
        json!(model.supports_search_tool),
    );
    object.insert(
        "supports_parallel_tool_calls".to_owned(),
        json!(model.supports_parallel_tool_calls),
    );
    insert_optional_string(object, "tool_mode", model.tool_mode.as_deref());
    object.insert(
        "experimental_supported_tools".to_owned(),
        json!(model.experimental_supported_tools),
    );
    insert_optional_string(
        object,
        "base_instructions",
        model.base_instructions.as_deref(),
    );
    insert_optional_string(
        object,
        "minimal_client_version",
        model.minimal_client_version.as_deref(),
    );
    object.insert("supported_in_api".to_owned(), json!(model.supported_in_api));
}

fn insert_optional<T: serde::Serialize>(
    object: &mut serde_json::Map<String, Value>,
    key: &str,
    value: Option<T>,
) {
    if let Some(value) = value {
        object.insert(key.to_owned(), json!(value));
    } else {
        object.remove(key);
    }
}

fn insert_optional_string(
    object: &mut serde_json::Map<String, Value>,
    key: &str,
    value: Option<&str>,
) {
    if let Some(value) = value {
        object.insert(key.to_owned(), Value::String(value.to_owned()));
    } else {
        object.remove(key);
    }
}

pub(super) fn read_catalog_pointer(config_path: &Path) -> Result<Option<PathBuf>, CatalogError> {
    let source = fs::read_to_string(config_path).map_err(CatalogError::Read)?;
    let document = source.parse::<DocumentMut>().map_err(CatalogError::Toml)?;
    Ok(document
        .get("model_catalog_json")
        .and_then(Item::as_str)
        .map(PathBuf::from))
}

pub(super) fn write_catalog_pointer(
    config_path: &Path,
    pointer: Option<&Path>,
) -> Result<(), CatalogError> {
    let source = fs::read_to_string(config_path).map_err(CatalogError::Read)?;
    let mut document = source.parse::<DocumentMut>().map_err(CatalogError::Toml)?;
    match pointer {
        Some(path) => {
            document.insert("model_catalog_json", value(path.display().to_string()));
        }
        None => {
            document.remove("model_catalog_json");
        }
    }
    write_atomic(config_path, document.to_string().as_bytes())
}

pub(super) fn read_record(path: &Path) -> Result<Option<OwnershipRecord>, CatalogError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(CatalogError::Read(error)),
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(CatalogError::Json)
}

pub(super) fn write_record(path: &Path, record: &OwnershipRecord) -> Result<(), CatalogError> {
    write_atomic(
        path,
        &serde_json::to_vec(record).map_err(CatalogError::Json)?,
    )
}

pub(super) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), CatalogError> {
    let parent = path
        .parent()
        .ok_or_else(|| CatalogError::Write(std::io::Error::other("target has no parent")))?;
    fs::create_dir_all(parent).map_err(CatalogError::Write)?;
    let mut temporary = NamedTempFile::new_in(parent).map_err(CatalogError::Write)?;
    temporary.write_all(bytes).map_err(CatalogError::Write)?;
    temporary
        .as_file()
        .sync_all()
        .map_err(CatalogError::Write)?;
    if let Ok(metadata) = fs::metadata(path) {
        temporary
            .as_file()
            .set_permissions(metadata.permissions())
            .map_err(CatalogError::Write)?;
    }
    temporary
        .persist(path)
        .map_err(|error| CatalogError::Write(error.error))?;
    Ok(())
}

pub(super) fn digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}
