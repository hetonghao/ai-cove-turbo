use std::{
    fs,
    path::{Path, PathBuf},
};

use serde_json::Value;

#[path = "catalog_diff.rs"]
mod catalog_diff;
#[path = "catalog_storage.rs"]
mod catalog_storage;
#[path = "catalog_types.rs"]
mod catalog_types;

use catalog_storage::{
    digest, parse_models, read_catalog_pointer, read_record, set_number, set_string,
    status_from_file, write_atomic, write_catalog_pointer, write_model_fields, write_record,
};
use catalog_types::OwnershipRecord;
pub(crate) use catalog_types::{
    CatalogChange, CatalogError, CatalogMetadata, CatalogModel, CatalogModelUpdate, CatalogStatus,
    ReasoningLevel, ServiceTier,
};

pub(crate) const FIXED_CATALOG_RELATIVE_PATH: &str = ".codex/model-catalogs/ai_cove_turbo.json";
const CODEX_TEMPLATE_SLUG: &str = "gpt-5.6-sol";

pub(crate) fn fixed_catalog_path(home: &Path) -> PathBuf {
    home.join(FIXED_CATALOG_RELATIVE_PATH)
}

pub(crate) fn save_metadata(home: &Path, metadata: &CatalogMetadata) -> Result<(), CatalogError> {
    catalog_storage::write_metadata(&fixed_catalog_path(home), metadata)
}

pub(crate) fn read_metadata(home: &Path) -> CatalogMetadata {
    catalog_storage::read_metadata(&fixed_catalog_path(home))
}

pub(crate) fn revision(bytes: &[u8]) -> String {
    catalog_storage::digest(bytes)
}

pub(crate) fn preview_catalog_models(
    home: &Path,
    config_path: &Path,
    recovery_path: &Path,
    models: &[CatalogModel],
    expected_revision: &str,
    removed_slugs: &[String],
) -> Result<(String, CatalogMetadata), CatalogError> {
    let fixed_path = fixed_catalog_path(home);
    let _record = read_record(recovery_path)?.ok_or(CatalogError::SourceUnavailable)?;
    let pointer = read_catalog_pointer(config_path)?;
    if pointer.as_deref() != Some(fixed_path.as_path()) {
        return Err(CatalogError::OwnershipConflict);
    }
    let bytes = fs::read(&fixed_path).map_err(CatalogError::Read)?;
    if digest(&bytes) != expected_revision {
        return Err(CatalogError::ContentChanged);
    }
    if let Some(slug) = protected_root_slug(&_record, removed_slugs) {
        return Err(CatalogError::ProtectedModel(slug));
    }
    let previous_metadata = catalog_storage::read_metadata(&fixed_path);
    let (next_bytes, metadata) =
        prepare_catalog_models(&bytes, &previous_metadata, models, removed_slugs)?;
    Ok((digest(&next_bytes), metadata))
}

pub(crate) fn model_from_discovery(value: &Value, slug: String) -> CatalogModel {
    catalog_storage::model_from_value(value, slug)
}

pub(crate) fn starting_status(home: &Path) -> CatalogStatus {
    CatalogStatus {
        path: fixed_catalog_path(home).display().to_string(),
        state: "starting".to_owned(),
        source_path: None,
        models: Vec::new(),
        changes: Vec::new(),
        restart_required: false,
        loaded: false,
        request_verified: false,
        revision: String::new(),
        metadata: CatalogMetadata::default(),
    }
}

pub(crate) fn ensure_catalog(
    home: &Path,
    config_path: &Path,
    recovery_path: &Path,
) -> Result<CatalogStatus, CatalogError> {
    let fixed_path = fixed_catalog_path(home);
    if let Some(record) = read_record(recovery_path)? {
        if record.fixed_path != fixed_path || record.config_path != config_path {
            return Err(CatalogError::OwnershipConflict);
        }
        let pointer = read_catalog_pointer(config_path)?;
        if pointer.as_deref() != Some(fixed_path.as_path()) {
            return Err(CatalogError::OwnershipConflict);
        }
        return status_from_file(&fixed_path, &record, false, true, false);
    }

    let current_pointer = read_catalog_pointer(config_path)?;
    let source_path = match current_pointer.as_ref() {
        Some(source) if source != &fixed_path => {
            let bytes = fs::read(source).map_err(CatalogError::Read)?;
            parse_models(&bytes)?;
            write_atomic(&fixed_path, &bytes)?;
            Some(source.clone())
        }
        Some(_) | None if fixed_path.exists() => None,
        Some(source) => {
            let bytes = fs::read(source).map_err(CatalogError::Read)?;
            parse_models(&bytes)?;
            write_atomic(&fixed_path, &bytes)?;
            Some(source.clone())
        }
        None => return Err(CatalogError::SourceUnavailable),
    };
    if source_path.is_none() && fixed_path.exists() {
        let bytes = fs::read(&fixed_path).map_err(CatalogError::Read)?;
        let _ = parse_models(&bytes)?;
    }

    let bytes = fs::read(&fixed_path).map_err(CatalogError::Read)?;
    let baseline_document: Value = serde_json::from_slice(&bytes).map_err(CatalogError::Json)?;
    let baseline_models = parse_models(&bytes)?;
    let record = OwnershipRecord {
        config_path: config_path.to_path_buf(),
        fixed_path: fixed_path.clone(),
        original_model_catalog_json: current_pointer
            .as_ref()
            .map(|path| path.display().to_string()),
        source_path,
        root_slugs: baseline_models
            .iter()
            .map(|model| model.slug.clone())
            .collect(),
        baseline_models,
        baseline_document,
    };
    write_record(recovery_path, &record)?;
    let changed_config = current_pointer.as_deref() != Some(fixed_path.as_path());
    if changed_config {
        if let Err(error) = write_catalog_pointer(config_path, Some(&fixed_path)) {
            let _ = fs::remove_file(recovery_path);
            return Err(error);
        }
    }
    status_from_file(&fixed_path, &record, changed_config, false, false)
}

pub(crate) fn reclaim_catalog(
    home: &Path,
    config_path: &Path,
    recovery_path: &Path,
) -> Result<CatalogStatus, CatalogError> {
    let fixed_path = fixed_catalog_path(home);
    let record = read_record(recovery_path)?.ok_or(CatalogError::SourceUnavailable)?;
    if record.config_path != config_path || record.fixed_path != fixed_path {
        return Err(CatalogError::OwnershipConflict);
    }
    write_catalog_pointer(config_path, Some(&fixed_path))?;
    status_from_file(&fixed_path, &record, true, false, false)
}

pub(crate) fn read_catalog(
    home: &Path,
    config_path: &Path,
    recovery_path: &Path,
    restart_required: bool,
    loaded: bool,
    request_verified: bool,
) -> Result<CatalogStatus, CatalogError> {
    let fixed_path = fixed_catalog_path(home);
    let record = read_record(recovery_path)?.ok_or(CatalogError::SourceUnavailable)?;
    if record.config_path != config_path || record.fixed_path != fixed_path {
        return Err(CatalogError::OwnershipConflict);
    }
    let pointer = read_catalog_pointer(config_path)?;
    if pointer.as_deref() != Some(fixed_path.as_path()) {
        return Err(CatalogError::OwnershipConflict);
    }
    status_from_file(
        &fixed_path,
        &record,
        restart_required,
        loaded,
        request_verified,
    )
}

pub(crate) fn update_catalog(
    home: &Path,
    config_path: &Path,
    recovery_path: &Path,
    updates: &[CatalogModelUpdate],
    expected_revision: &str,
) -> Result<CatalogStatus, CatalogError> {
    let fixed_path = fixed_catalog_path(home);
    let record = read_record(recovery_path)?.ok_or(CatalogError::SourceUnavailable)?;
    let pointer = read_catalog_pointer(config_path)?;
    if pointer.as_deref() != Some(fixed_path.as_path()) {
        return Err(CatalogError::OwnershipConflict);
    }
    let bytes = fs::read(&fixed_path).map_err(CatalogError::Read)?;
    if digest(&bytes) != expected_revision {
        return Err(CatalogError::ContentChanged);
    }
    let mut document: Value = serde_json::from_slice(&bytes).map_err(CatalogError::Json)?;
    let models = document
        .get_mut("models")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| CatalogError::InvalidSchema("缺少 models 数组".to_owned()))?;
    let mut seen = std::collections::HashSet::new();
    for update in updates {
        if !seen.insert(update.slug.clone()) {
            return Err(CatalogError::InvalidModel(update.slug.clone()));
        }
        let Some(model) = models
            .iter_mut()
            .find(|model| model.get("slug").and_then(Value::as_str) == Some(update.slug.as_str()))
        else {
            return Err(CatalogError::InvalidModel(update.slug.clone()));
        };
        if let Some(display_name) = &update.display_name {
            if display_name.trim().is_empty() {
                return Err(CatalogError::InvalidSchema(format!(
                    "模型 {} 的 display_name 不能为空",
                    update.slug
                )));
            }
            set_string(model, "display_name", display_name);
        }
        if let Some(description) = &update.description {
            set_string(model, "description", description);
        }
        if let Some(visibility) = &update.visibility {
            let current_visibility = model
                .get("visibility")
                .and_then(Value::as_str)
                .unwrap_or("list");
            if visibility != current_visibility
                && !matches!(visibility.as_str(), "list" | "hide" | "none")
            {
                return Err(CatalogError::InvalidSchema(format!(
                    "模型 {} 的 visibility 无效",
                    update.slug
                )));
            }
            set_string(model, "visibility", visibility);
        }
        if let Some(priority) = update.priority {
            set_number(model, "priority", priority);
        }
    }
    models.sort_by(|left, right| {
        let left_priority = left
            .get("priority")
            .and_then(Value::as_i64)
            .unwrap_or_default();
        let right_priority = right
            .get("priority")
            .and_then(Value::as_i64)
            .unwrap_or_default();
        left_priority.cmp(&right_priority).then_with(|| {
            left.get("slug")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .cmp(
                    right
                        .get("slug")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                )
        })
    });
    // ponytail: legacy catalogs get a one-time safe-field migration before validation.
    migrate_legacy_models(models);
    validate_codex_document(&document)?;
    write_atomic(
        &fixed_path,
        &serde_json::to_vec_pretty(&document).map_err(CatalogError::Json)?,
    )?;
    status_from_file(&fixed_path, &record, true, false, false)
}

pub(crate) fn save_catalog_models(
    home: &Path,
    config_path: &Path,
    recovery_path: &Path,
    models: &[CatalogModel],
    expected_revision: &str,
) -> Result<CatalogStatus, CatalogError> {
    save_catalog_models_with_removals(
        home,
        config_path,
        recovery_path,
        models,
        expected_revision,
        &[],
    )
}

pub(crate) fn save_catalog_models_with_removals(
    home: &Path,
    config_path: &Path,
    recovery_path: &Path,
    models: &[CatalogModel],
    expected_revision: &str,
    removed_slugs: &[String],
) -> Result<CatalogStatus, CatalogError> {
    let fixed_path = fixed_catalog_path(home);
    let record = read_record(recovery_path)?.ok_or(CatalogError::SourceUnavailable)?;
    let pointer = read_catalog_pointer(config_path)?;
    if pointer.as_deref() != Some(fixed_path.as_path()) {
        return Err(CatalogError::OwnershipConflict);
    }
    let bytes = fs::read(&fixed_path).map_err(CatalogError::Read)?;
    if digest(&bytes) != expected_revision {
        return Err(CatalogError::ContentChanged);
    }
    if let Some(slug) = protected_root_slug(&record, removed_slugs) {
        return Err(CatalogError::ProtectedModel(slug));
    }
    let previous_metadata = catalog_storage::read_metadata(&fixed_path);
    let (next_bytes, metadata) =
        prepare_catalog_models(&bytes, &previous_metadata, models, removed_slugs)?;
    write_atomic(&fixed_path, &next_bytes)?;
    if let Err(error) = catalog_storage::write_metadata(&fixed_path, &metadata) {
        let _ = write_atomic(&fixed_path, &bytes);
        let _ = catalog_storage::write_metadata(&fixed_path, &previous_metadata);
        return Err(error);
    }
    status_from_file(&fixed_path, &record, true, false, false)
}

fn prepare_catalog_models(
    bytes: &[u8],
    previous_metadata: &CatalogMetadata,
    models: &[CatalogModel],
    removed_slugs: &[String],
) -> Result<(Vec<u8>, CatalogMetadata), CatalogError> {
    let normalized_models = models
        .iter()
        .cloned()
        .map(CatalogModel::with_safe_defaults)
        .collect::<Vec<_>>();
    let mut document: Value = serde_json::from_slice(bytes).map_err(CatalogError::Json)?;
    let entries = document
        .get_mut("models")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| CatalogError::InvalidSchema("缺少 models 数组".to_owned()))?;
    migrate_legacy_models(entries);
    remove_catalog_entries(entries, removed_slugs);
    let needs_template = models.iter().any(|model| {
        !entries
            .iter()
            .any(|entry| entry.get("slug").and_then(Value::as_str) == Some(model.slug.as_str()))
    });
    let template = needs_template
        .then(|| {
            entries
                .iter()
                .find(|entry| {
                    entry.get("slug").and_then(Value::as_str) == Some(CODEX_TEMPLATE_SLUG)
                })
                .cloned()
                .ok_or_else(|| {
                    CatalogError::InvalidSchema(format!(
                        "新增模型缺少 {CODEX_TEMPLATE_SLUG} 基准模板"
                    ))
                })
        })
        .transpose()?;
    let mut seen = std::collections::HashSet::new();
    for model in &normalized_models {
        model
            .validate_complete()
            .map_err(CatalogError::InvalidSchema)?;
        if !seen.insert(model.slug.clone()) {
            return Err(CatalogError::InvalidModel(model.slug.clone()));
        }
        if let Some(existing) = entries
            .iter_mut()
            .find(|entry| entry.get("slug").and_then(Value::as_str) == Some(model.slug.as_str()))
        {
            write_model_fields(existing, model);
        } else {
            let Some(template) = template.as_ref() else {
                return Err(CatalogError::InvalidSchema(format!(
                    "新增模型缺少 {CODEX_TEMPLATE_SLUG} 基准模板"
                )));
            };
            let mut created = template.clone();
            write_model_fields(&mut created, model);
            sanitize_new_model_template(&mut created, template, model);
            entries.push(created);
        }
    }
    entries.sort_by(|left, right| {
        let left_priority = left
            .get("priority")
            .and_then(Value::as_i64)
            .unwrap_or_default();
        let right_priority = right
            .get("priority")
            .and_then(Value::as_i64)
            .unwrap_or_default();
        left_priority.cmp(&right_priority).then_with(|| {
            left.get("slug")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .cmp(
                    right
                        .get("slug")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                )
        })
    });
    validate_codex_document(&document)?;
    let mut metadata = previous_metadata.clone();
    remove_catalog_metadata(&mut metadata, removed_slugs);
    for model in &normalized_models {
        metadata
            .field_sources
            .insert(model.slug.clone(), model.field_sources.clone());
        if model.conflicts.is_empty() {
            metadata.conflicts.remove(&model.slug);
        } else {
            metadata
                .conflicts
                .insert(model.slug.clone(), model.conflicts.clone());
        }
    }
    Ok((
        serde_json::to_vec_pretty(&document).map_err(CatalogError::Json)?,
        metadata,
    ))
}

fn remove_catalog_entries(entries: &mut Vec<Value>, removed_slugs: &[String]) {
    entries.retain(|entry| {
        entry
            .get("slug")
            .and_then(Value::as_str)
            .is_none_or(|slug| !removed_slugs.iter().any(|removed| removed == slug))
    });
}

fn protected_root_slug(record: &OwnershipRecord, removed_slugs: &[String]) -> Option<String> {
    let root_slugs = record
        .source_path
        .as_ref()
        .and_then(|path| fs::read(path).ok())
        .and_then(|bytes| parse_models(&bytes).ok())
        .map(|models| {
            models
                .into_iter()
                .map(|model| model.slug)
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|| {
            if record.root_slugs.is_empty() {
                record
                    .baseline_models
                    .iter()
                    .map(|model| model.slug.clone())
                    .collect()
            } else {
                record.root_slugs.clone()
            }
        });
    removed_slugs
        .iter()
        .find(|slug| root_slugs.iter().any(|root_slug| root_slug == *slug))
        .cloned()
}

fn remove_catalog_metadata(metadata: &mut CatalogMetadata, removed_slugs: &[String]) {
    for slug in removed_slugs {
        metadata.field_sources.remove(slug);
        metadata.conflicts.remove(slug);
    }
}

fn sanitize_new_model_template(model: &mut Value, template: &Value, target: &CatalogModel) {
    let Some(object) = model.as_object_mut() else {
        return;
    };
    object.remove("additional_speed_tiers");
    object.remove("apply_patch_tool_type");
    object.remove("availability_nux");
    object.remove("upgrade");
    if !CatalogModel::is_gpt_like_slug(&target.slug) {
        object.remove("apply_patch_tool_type");
        object.remove("additional_speed_tiers");
        object.insert("service_tiers".to_owned(), serde_json::json!([]));
        object.remove("default_service_tier");
        object.insert(
            "supports_reasoning_summary_parameter".to_owned(),
            serde_json::Value::Bool(false),
        );
        object.insert(
            "default_reasoning_summary".to_owned(),
            serde_json::Value::String("none".to_owned()),
        );
    }
    if let Some(instructions) = template.get("base_instructions").and_then(Value::as_str) {
        object.insert(
            "base_instructions".to_owned(),
            Value::String(replace_template_model_name(instructions, template, target)),
        );
    }
    if let Some(messages) = object
        .get_mut("model_messages")
        .and_then(Value::as_object_mut)
    {
        if let Some(instructions) = template
            .get("model_messages")
            .and_then(Value::as_object)
            .and_then(|messages| messages.get("instructions_template"))
            .and_then(Value::as_str)
        {
            messages.insert(
                "instructions_template".to_owned(),
                Value::String(replace_template_model_name(instructions, template, target)),
            );
        }
    }
}

fn replace_template_model_name(text: &str, template: &Value, target: &CatalogModel) -> String {
    let target_name = if target.display_name.trim().is_empty() {
        target.slug.trim()
    } else {
        target.display_name.trim()
    };
    let sources = [
        Some("GPT-5.6-Sol"),
        Some("GPT-5.6 Sol"),
        Some("gpt-5.6-sol"),
        Some("GPT-5.6"),
        Some("GPT-5"),
        template.get("display_name").and_then(Value::as_str),
        template.get("slug").and_then(Value::as_str),
    ];
    let mut result = text.to_owned();
    let mut markers = Vec::new();
    for (index, source) in sources.into_iter().enumerate() {
        let Some(source) = source.filter(|value| !value.is_empty()) else {
            continue;
        };
        if !result.contains(source) {
            continue;
        }
        let mut marker = format!("__AI_COVE_MODEL_NAME_{index}__");
        while result.contains(&marker) || target_name.contains(&marker) {
            marker.push('_');
        }
        result = result.replace(source, &marker);
        markers.push(marker);
    }
    for marker in markers {
        result = result.replace(&marker, target_name);
    }
    result
}

fn validate_codex_document(document: &Value) -> Result<(), CatalogError> {
    let models = document
        .get("models")
        .and_then(Value::as_array)
        .ok_or_else(|| CatalogError::InvalidSchema("缺少 models 数组".to_owned()))?;
    let mut seen = std::collections::HashSet::new();
    for model in models {
        let slug = model
            .get("slug")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| CatalogError::InvalidSchema("模型缺少 slug".to_owned()))?;
        if !seen.insert(slug.to_owned()) {
            return Err(CatalogError::InvalidSchema(format!(
                "模型 slug 重复：{slug}"
            )));
        }
        if !model.get("truncation_policy").is_some_and(|policy| {
            let Some(object) = policy.as_object() else {
                return false;
            };
            object
                .get("mode")
                .and_then(Value::as_str)
                .is_some_and(|mode| matches!(mode, "tokens" | "bytes"))
                && object
                    .get("limit")
                    .and_then(Value::as_u64)
                    .is_some_and(|limit| limit > 0)
        }) {
            return Err(CatalogError::InvalidSchema(format!(
                "模型 {slug} 缺少有效 truncation_policy"
            )));
        }
        if model
            .get("shell_type")
            .and_then(Value::as_str)
            .is_none_or(|value| value.trim().is_empty())
        {
            return Err(CatalogError::InvalidSchema(format!(
                "模型 {slug} 缺少 shell_type"
            )));
        }
        if !model
            .get("support_verbosity")
            .is_some_and(Value::is_boolean)
        {
            return Err(CatalogError::InvalidSchema(format!(
                "模型 {slug} 缺少 support_verbosity"
            )));
        }
    }
    Ok(())
}

fn migrate_legacy_models(models: &mut [Value]) {
    let legacy = models.iter().all(|model| {
        model.as_object().is_some_and(|object| {
            !object.contains_key("truncation_policy")
                && !object.contains_key("shell_type")
                && !object.contains_key("support_verbosity")
        })
    });
    if !legacy {
        return;
    }
    for model in models {
        let Some(object) = model.as_object_mut() else {
            continue;
        };
        object.insert(
            "truncation_policy".to_owned(),
            serde_json::json!({"mode": "tokens", "limit": 10_000}),
        );
        object.insert(
            "shell_type".to_owned(),
            Value::String("shell_command".to_owned()),
        );
        object.insert("support_verbosity".to_owned(), Value::Bool(true));
    }
}

pub(crate) fn restore_catalog_bytes(
    home: &Path,
    config_path: &Path,
    recovery_path: &Path,
    expected_revision: &str,
    bytes: &[u8],
) -> Result<CatalogStatus, CatalogError> {
    let fixed_path = fixed_catalog_path(home);
    let record = read_record(recovery_path)?.ok_or(CatalogError::SourceUnavailable)?;
    let pointer = read_catalog_pointer(config_path)?;
    if pointer.as_deref() != Some(fixed_path.as_path()) {
        return Err(CatalogError::OwnershipConflict);
    }
    let current = fs::read(&fixed_path).map_err(CatalogError::Read)?;
    if digest(&current) != expected_revision {
        return Err(CatalogError::ContentChanged);
    }
    write_atomic(&fixed_path, bytes)?;
    status_from_file(&fixed_path, &record, true, false, false)
}

pub(crate) fn restore_catalog(
    home: &Path,
    config_path: &Path,
    recovery_path: &Path,
) -> Result<CatalogStatus, CatalogError> {
    let fixed_path = fixed_catalog_path(home);
    let Some(record) = read_record(recovery_path)? else {
        return Err(CatalogError::SourceUnavailable);
    };
    let pointer = read_catalog_pointer(config_path)?;
    if pointer.as_deref() == Some(fixed_path.as_path()) {
        write_catalog_pointer(
            config_path,
            record.original_model_catalog_json.as_deref().map(Path::new),
        )?;
    }
    match fs::remove_file(recovery_path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(CatalogError::Write(error)),
    }
    Ok(CatalogStatus {
        path: fixed_path.display().to_string(),
        state: "restored".to_owned(),
        source_path: record
            .source_path
            .as_ref()
            .map(|path| path.display().to_string()),
        models: record.baseline_models,
        changes: Vec::new(),
        restart_required: false,
        loaded: false,
        request_verified: false,
        revision: String::new(),
        metadata: CatalogMetadata::default(),
    })
}

#[cfg(test)]
mod tests {
    use std::{error::Error, fs};

    use tempfile::tempdir;

    use super::*;

    fn fixture(root: &Path, pointer: Option<&Path>) -> (PathBuf, PathBuf) {
        let config = root.join("config.toml");
        let source = root.join("source.json");
        fs::write(
            &config,
            pointer.map_or_else(
                || "model_provider = \"custom\"\n".to_owned(),
                |path| {
                    format!(
                        "model_provider = \"custom\"\nmodel_catalog_json = \"{}\"\n",
                        path.display()
                    )
                },
            ),
        )
        .expect("config fixture");
        fs::write(
            &source,
            r#"{"models":[{"slug":"alpha","display_name":"Alpha","description":"a","visibility":"list","priority":2,"unknown":"kept","truncation_policy":{"mode":"bytes","limit":10000},"shell_type":"shell_command","support_verbosity":true},{"slug":"beta","display_name":"Beta","description":"b","visibility":"hide","priority":1,"truncation_policy":{"mode":"bytes","limit":10000},"shell_type":"shell_command","support_verbosity":true},{"slug":"gpt-5.6-sol","display_name":"GPT-5.6-Sol","description":"Codex template","visibility":"list","priority":0,"template_only":"kept","truncation_policy":{"mode":"tokens","limit":10000},"shell_type":"shell_command","support_verbosity":true}]}"#,
        )
        .expect("catalog fixture");
        (config, source)
    }

    #[test]
    fn external_source_is_copied_and_fixed_pointer_is_owned() -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let source = root.path().join("source.json");
        let (config, _) = fixture(root.path(), Some(&source));
        let recovery = root.path().join("recovery.json");
        let status = ensure_catalog(root.path(), &config, &recovery)?;
        assert_eq!(status.state, "owned");
        assert_eq!(
            status.source_path.as_deref(),
            Some(source.to_string_lossy().as_ref())
        );
        assert!(fixed_catalog_path(root.path()).exists());
        assert!(fs::read_to_string(config)?.contains("ai_cove_turbo.json"));
        assert!(fs::read_to_string(fixed_catalog_path(root.path()))?.contains("unknown"));
        Ok(())
    }

    #[test]
    fn fixed_catalog_reuse_does_not_require_a_source_file() -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let (config, _) = fixture(root.path(), None);
        let fixed = fixed_catalog_path(root.path());
        fs::create_dir_all(fixed.parent().expect("catalog parent"))?;
        fs::write(&fixed, r#"{"models":[]}"#)?;
        let status = ensure_catalog(root.path(), &config, &root.path().join("recovery.json"))?;
        assert_eq!(status.models, Vec::<CatalogModel>::new());
        assert!(fs::read_to_string(config)?.contains("model_catalog_json"));
        Ok(())
    }

    #[test]
    fn relative_catalog_pointer_resolves_from_codex_config_directory() -> Result<(), Box<dyn Error>>
    {
        let root = tempdir()?;
        let config_dir = root.path().join("home/.codex");
        fs::create_dir_all(&config_dir)?;
        let source = config_dir.join("models.json");
        fs::write(&source, r#"{"models":[]}"#)?;
        let config = config_dir.join("config.toml");
        fs::write(
            &config,
            "model_provider = \"custom\"\nmodel_catalog_json = \"models.json\"\n",
        )?;

        let recovery = root.path().join("recovery.json");
        let home = root.path().join("home");
        let status = ensure_catalog(&home, &config, &recovery)?;

        assert_eq!(
            status.source_path,
            Some(source.to_string_lossy().into_owned())
        );
        assert_eq!(status.models.len(), 0);
        Ok(())
    }

    #[test]
    fn canonical_template_cannot_be_deleted() -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let source = root.path().join("source.json");
        let (config, _) = fixture(root.path(), Some(&source));
        let recovery = root.path().join("recovery.json");
        let current = ensure_catalog(root.path(), &config, &recovery)?;

        let result = save_catalog_models_with_removals(
            root.path(),
            &config,
            &recovery,
            &[],
            &current.revision,
            &[CODEX_TEMPLATE_SLUG.to_owned()],
        );

        assert!(
            matches!(result, Err(CatalogError::ProtectedModel(slug)) if slug == CODEX_TEMPLATE_SLUG)
        );
        assert!(fs::read_to_string(fixed_catalog_path(root.path()))?.contains(CODEX_TEMPLATE_SLUG));
        Ok(())
    }

    #[test]
    fn every_current_root_model_is_protected_from_deletion() -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let source = root.path().join("source.json");
        let (config, _) = fixture(root.path(), Some(&source));
        let recovery = root.path().join("recovery.json");
        let current = ensure_catalog(root.path(), &config, &recovery)?;

        let result = save_catalog_models_with_removals(
            root.path(),
            &config,
            &recovery,
            &[],
            &current.revision,
            &["alpha".to_owned()],
        );

        assert!(matches!(result, Err(CatalogError::ProtectedModel(slug)) if slug == "alpha"));
        Ok(())
    }

    #[test]
    fn catalog_status_marks_root_and_added_models() -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let source = root.path().join("source.json");
        let (config, _) = fixture(root.path(), Some(&source));
        let recovery = root.path().join("recovery.json");
        let current = ensure_catalog(root.path(), &config, &recovery)?;
        let saved = save_catalog_models(
            root.path(),
            &config,
            &recovery,
            &[complete_model("gamma")],
            &current.revision,
        )?;

        let alpha = saved
            .models
            .iter()
            .find(|model| model.slug == "alpha")
            .ok_or("alpha missing")?;
        let gamma = saved
            .models
            .iter()
            .find(|model| model.slug == "gamma")
            .ok_or("gamma missing")?;
        assert!(alpha.root_presence);
        assert!(!alpha.root_missing);
        assert!(!gamma.root_presence);
        assert!(gamma.root_missing);

        fs::write(
            &source,
            r#"{"models":[{"slug":"beta","display_name":"Beta","visibility":"hide","priority":1},{"slug":"gpt-5.6-sol","display_name":"GPT-5.6-Sol","visibility":"list","priority":0}]}"#,
        )?;
        let refreshed = read_catalog(root.path(), &config, &recovery, true, false, false)?;
        let alpha = refreshed
            .models
            .iter()
            .find(|model| model.slug == "alpha")
            .ok_or("alpha missing after source refresh")?;
        assert!(!alpha.root_presence);
        assert!(alpha.root_missing);
        Ok(())
    }

    #[test]
    fn external_source_refreshes_an_existing_fixed_catalog_before_takeover()
    -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let source = root.path().join("source.json");
        let (config, _) = fixture(root.path(), Some(&source));
        let fixed = fixed_catalog_path(root.path());
        fs::create_dir_all(fixed.parent().expect("catalog parent"))?;
        fs::write(
            &fixed,
            r#"{"models":[{"slug":"stale","display_name":"Stale","visibility":"hide","priority":9}]}"#,
        )?;
        let recovery = root.path().join("recovery.json");

        let status = ensure_catalog(root.path(), &config, &recovery)?;

        assert_eq!(
            status.source_path.as_deref(),
            Some(source.to_string_lossy().as_ref())
        );
        assert_eq!(
            status.models.first().map(|model| model.slug.as_str()),
            Some("alpha")
        );
        assert!(fs::read_to_string(fixed)?.contains("\"beta\""));
        assert!(fs::read_to_string(config)?.contains("ai_cove_turbo.json"));
        Ok(())
    }

    #[test]
    fn external_pointer_edit_is_conflict_and_recovery_reclaims_it() -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let source = root.path().join("source.json");
        let (config, _) = fixture(root.path(), Some(&source));
        let recovery = root.path().join("recovery.json");
        ensure_catalog(root.path(), &config, &recovery)?;
        let external = root.path().join("external.json");
        fs::copy(&source, &external)?;
        write_catalog_pointer(&config, Some(&external))?;
        assert!(matches!(
            read_catalog(root.path(), &config, &recovery, false, false, false),
            Err(CatalogError::OwnershipConflict)
        ));
        let status = ensure_catalog(root.path(), &config, &recovery);
        assert!(matches!(status, Err(CatalogError::OwnershipConflict)));
        let reclaimed = reclaim_catalog(root.path(), &config, &recovery)?;
        assert!(reclaimed.restart_required);
        assert!(fs::read_to_string(config)?.contains("ai_cove_turbo.json"));
        Ok(())
    }

    #[test]
    fn update_preserves_unknown_fields_and_records_stable_diff() -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let source = root.path().join("source.json");
        let (config, _) = fixture(root.path(), Some(&source));
        let recovery = root.path().join("recovery.json");
        let current = ensure_catalog(root.path(), &config, &recovery)?;
        let status = update_catalog(
            root.path(),
            &config,
            &recovery,
            &[CatalogModelUpdate {
                slug: "alpha".to_owned(),
                display_name: None,
                description: None,
                visibility: Some("hide".to_owned()),
                priority: Some(9),
            }],
            &current.revision,
        )?;
        assert!(status.restart_required);
        assert_eq!(status.changes.len(), 2);
        let written = fs::read_to_string(fixed_catalog_path(root.path()))?;
        assert!(written.contains("unknown"));
        assert!(written.contains("\"priority\": 9"));
        Ok(())
    }

    #[test]
    fn save_rejects_a_stale_catalog_revision() -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let source = root.path().join("source.json");
        let (config, _) = fixture(root.path(), Some(&source));
        let recovery = root.path().join("recovery.json");
        let status = ensure_catalog(root.path(), &config, &recovery)?;
        fs::write(fixed_catalog_path(root.path()), r#"{"models":[]}"#)?;
        let result = update_catalog(root.path(), &config, &recovery, &[], &status.revision);
        assert!(matches!(result, Err(CatalogError::ContentChanged)));
        Ok(())
    }

    #[test]
    fn legacy_model_can_update_display_fields_without_claiming_capabilities()
    -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let source = root.path().join("source.json");
        let (config, _) = fixture(root.path(), Some(&source));
        let recovery = root.path().join("recovery.json");
        let current = ensure_catalog(root.path(), &config, &recovery)?;
        let status = update_catalog(
            root.path(),
            &config,
            &recovery,
            &[CatalogModelUpdate {
                slug: "alpha".to_owned(),
                display_name: Some("Alpha renamed".to_owned()),
                description: Some("new description".to_owned()),
                visibility: None,
                priority: None,
            }],
            &current.revision,
        )?;
        let alpha = status.models.iter().find(|model| model.slug == "alpha");
        assert_eq!(
            alpha.map(|model| model.display_name.as_str()),
            Some("Alpha renamed")
        );
        assert_eq!(
            alpha.map(|model| model.description.as_str()),
            Some("new description")
        );
        assert!(alpha.is_some_and(|model| model.max_context_window.is_none()));
        let written = fs::read_to_string(fixed_catalog_path(root.path()))?;
        assert!(written.contains("truncation_policy"));
        assert!(written.contains("shell_type"));
        assert!(written.contains("support_verbosity"));
        Ok(())
    }

    #[test]
    fn catalog_diff_reports_known_unknown_added_and_removed_content() -> Result<(), Box<dyn Error>>
    {
        let root = tempdir()?;
        let source = root.path().join("source.json");
        let (config, _) = fixture(root.path(), Some(&source));
        let recovery = root.path().join("recovery.json");
        ensure_catalog(root.path(), &config, &recovery)?;
        fs::write(
            fixed_catalog_path(root.path()),
            r#"{"models":[{"slug":"alpha","display_name":"Alpha v2","description":"a2","visibility":"list","priority":2,"unknown":"changed","truncation_policy":{"mode":"bytes","limit":10000},"shell_type":"shell_command","support_verbosity":true},{"slug":"gamma","display_name":"Gamma","description":"g","visibility":"list","priority":3,"new_field":{"nested":true},"truncation_policy":{"mode":"bytes","limit":10000},"shell_type":"shell_command","support_verbosity":true}]}"#,
        )?;

        let status = read_catalog(root.path(), &config, &recovery, true, false, false)?;
        let fields = status
            .changes
            .iter()
            .map(|change| (change.slug.as_str(), change.field.as_str()))
            .collect::<std::collections::HashSet<_>>();
        assert!(fields.contains(&("alpha", "display_name")));
        assert!(fields.contains(&("alpha", "description")));
        assert!(fields.contains(&("alpha", "unknown")));
        assert!(fields.contains(&("beta", "model")));
        assert!(fields.contains(&("gamma", "model")));
        Ok(())
    }

    fn complete_model(slug: &str) -> CatalogModel {
        let mut model = CatalogModel::basic(slug.to_owned());
        model.context_window = Some(125_000);
        model.max_context_window = Some(250_000);
        model.auto_compact_token_limit = Some(112_500);
        model
            .field_sources
            .insert("contextWindow".to_owned(), "用户".to_owned());
        model
            .field_sources
            .insert("maxContextWindow".to_owned(), "用户".to_owned());
        model.supported_reasoning_levels = vec![catalog_types::ReasoningLevel {
            effort: "low".to_owned(),
            description: "低延迟".to_owned(),
        }];
        model.default_reasoning_level = Some("low".to_owned());
        model
    }

    #[test]
    fn explicit_model_removal_does_not_leave_the_deleted_candidate_in_catalog()
    -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let source = root.path().join("source.json");
        let (config, _) = fixture(root.path(), Some(&source));
        let recovery = root.path().join("recovery.json");
        let current = ensure_catalog(root.path(), &config, &recovery)?;
        let seeded = save_catalog_models(
            root.path(),
            &config,
            &recovery,
            &[complete_model("gamma")],
            &current.revision,
        )?;
        let removed = vec!["gamma".to_owned()];

        let status = save_catalog_models_with_removals(
            root.path(),
            &config,
            &recovery,
            &[complete_model("beta")],
            &seeded.revision,
            &removed,
        )?;

        assert!(status.models.iter().all(|model| model.slug != "gamma"));
        let written: Value = serde_json::from_slice(&fs::read(fixed_catalog_path(root.path()))?)?;
        let models = written
            .get("models")
            .and_then(Value::as_array)
            .ok_or("models missing")?;
        assert!(
            models
                .iter()
                .all(|model| model.get("slug") != Some(&Value::String("gamma".to_owned())))
        );
        Ok(())
    }

    #[test]
    fn complete_model_save_upserts_without_dropping_unknown_fields() -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let source = root.path().join("source.json");
        let (config, _) = fixture(root.path(), Some(&source));
        let recovery = root.path().join("recovery.json");
        let current = ensure_catalog(root.path(), &config, &recovery)?;
        let mut model = complete_model("alpha");
        model.display_name = "Alpha updated".to_owned();
        model.truncation_policy = Some(serde_json::json!({"mode": "bytes", "limit": 10000}));
        let added = complete_model("gamma");
        let status = save_catalog_models(
            root.path(),
            &config,
            &recovery,
            &[model, added],
            &current.revision,
        )?;
        assert_eq!(status.models.len(), 4);
        assert_eq!(
            status
                .models
                .iter()
                .find(|model| model.slug == "gamma")
                .and_then(|model| model.field_sources.get("maxContextWindow"))
                .map(String::as_str),
            Some("用户")
        );
        let written = fs::read_to_string(fixed_catalog_path(root.path()))?;
        assert!(written.contains("unknown"));
        assert!(written.contains("\"max_context_window\": 250000"));
        assert!(written.contains("\"slug\": \"gamma\""));
        let document: Value = serde_json::from_str(&written)?;
        let gamma = document
            .get("models")
            .and_then(Value::as_array)
            .and_then(|models| {
                models
                    .iter()
                    .find(|model| model.get("slug") == Some(&Value::String("gamma".to_owned())))
            })
            .ok_or("gamma missing")?;
        assert_eq!(
            gamma.get("truncation_policy"),
            Some(&serde_json::json!({"mode": "tokens", "limit": 10000}))
        );
        assert_eq!(
            gamma.get("shell_type").and_then(Value::as_str),
            Some("shell_command")
        );
        assert_eq!(
            gamma.get("support_verbosity").and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            gamma.get("template_only").and_then(Value::as_str),
            Some("kept")
        );
        let alpha = document
            .get("models")
            .and_then(Value::as_array)
            .and_then(|models| {
                models
                    .iter()
                    .find(|model| model.get("slug") == Some(&Value::String("alpha".to_owned())))
            })
            .ok_or("alpha missing")?;
        assert_eq!(
            alpha.get("truncation_policy"),
            Some(&serde_json::json!({"mode": "bytes", "limit": 10000}))
        );
        assert!(
            root.path()
                .join(".codex/model-catalogs/ai_cove_turbo.metadata.json")
                .exists()
        );
        Ok(())
    }

    #[test]
    fn save_rejects_catalog_missing_codex_fields_before_replacing_file()
    -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let source = root.path().join("source.json");
        let (config, _) = fixture(root.path(), Some(&source));
        let recovery = root.path().join("recovery.json");
        ensure_catalog(root.path(), &config, &recovery)?;

        let fixed = fixed_catalog_path(root.path());
        let mut document: Value = serde_json::from_slice(&fs::read(&fixed)?)?;
        document["models"][1]
            .as_object_mut()
            .expect("beta object")
            .remove("truncation_policy");
        fs::write(&fixed, serde_json::to_vec_pretty(&document)?)?;
        let broken = fs::read(&fixed)?;
        let result = save_catalog_models(
            root.path(),
            &config,
            &recovery,
            &[complete_model("alpha")],
            &revision(&broken),
        );

        assert!(
            matches!(result, Err(CatalogError::InvalidSchema(message)) if message.contains("beta") && message.contains("truncation_policy"))
        );
        assert_eq!(fs::read(fixed)?, broken);
        Ok(())
    }

    #[test]
    fn saving_a_legacy_catalog_migrates_missing_codex_fields_once() -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let config = root.path().join("config.toml");
        let source = root.path().join("source.json");
        fs::write(
            &config,
            format!(
                "model_provider = \"custom\"\nmodel_catalog_json = \"{}\"\n",
                source.display()
            ),
        )?;
        fs::write(
            &source,
            r#"{"models":[{"slug":"alpha","visibility":"list","priority":1}]}"#,
        )?;
        let recovery = root.path().join("recovery.json");
        let initial = ensure_catalog(root.path(), &config, &recovery)?;
        let status = save_catalog_models(
            root.path(),
            &config,
            &recovery,
            &[complete_model("alpha")],
            &initial.revision,
        )?;
        assert_eq!(status.models.len(), 1);
        let written = fs::read_to_string(fixed_catalog_path(root.path()))?;
        assert!(written.contains("truncation_policy"));
        assert!(written.contains("shell_type"));
        assert!(written.contains("support_verbosity"));
        Ok(())
    }

    #[test]
    fn update_rejects_catalog_missing_codex_fields_before_replacing_file()
    -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let source = root.path().join("source.json");
        let (config, _) = fixture(root.path(), Some(&source));
        let recovery = root.path().join("recovery.json");
        ensure_catalog(root.path(), &config, &recovery)?;

        let fixed = fixed_catalog_path(root.path());
        let mut document: Value = serde_json::from_slice(&fs::read(&fixed)?)?;
        document["models"][1]
            .as_object_mut()
            .expect("beta object")
            .remove("shell_type");
        fs::write(&fixed, serde_json::to_vec_pretty(&document)?)?;
        let broken = fs::read(&fixed)?;
        let result = update_catalog(
            root.path(),
            &config,
            &recovery,
            &[CatalogModelUpdate {
                slug: "alpha".to_owned(),
                display_name: Some("Alpha updated".to_owned()),
                description: None,
                visibility: None,
                priority: None,
            }],
            &revision(&broken),
        );

        assert!(
            matches!(result, Err(CatalogError::InvalidSchema(message)) if message.contains("beta") && message.contains("shell_type"))
        );
        assert_eq!(fs::read(fixed)?, broken);
        Ok(())
    }

    #[test]
    fn new_model_uses_the_gpt_5_6_sol_entry_as_template() -> Result<(), Box<dyn Error>> {
        let bytes = br#"{"models":[{"slug":"gpt-5.6-sol","display_name":"Sol","description":"template","visibility":"list","priority":1,"context_window":125000,"max_context_window":250000,"supported_reasoning_levels":[{"effort":"low"}],"default_reasoning_level":"low","truncation_policy":{"mode":"tokens","limit":10000},"shell_type":"shell_command","support_verbosity":true,"template_only":"kept"}]}"#;
        let model = complete_model("gamma");
        let (next, _) = prepare_catalog_models(bytes, &CatalogMetadata::default(), &[model], &[])?;
        let document: Value = serde_json::from_slice(&next)?;
        let gamma = document["models"]
            .as_array()
            .and_then(|models| models.iter().find(|model| model["slug"] == "gamma"))
            .ok_or("gamma missing")?;
        assert_eq!(gamma["template_only"], "kept");
        Ok(())
    }

    #[test]
    fn new_model_does_not_inherit_template_only_identity_metadata() -> Result<(), Box<dyn Error>> {
        let bytes = br#"{"models":[{"slug":"gpt-5.6-sol","display_name":"GPT-5.6-Sol","description":"template","visibility":"list","priority":1,"context_window":125000,"max_context_window":250000,"supported_reasoning_levels":[{"effort":"low"}],"default_reasoning_level":"low","truncation_policy":{"mode":"tokens","limit":10000},"shell_type":"shell_command","support_verbosity":true,"base_instructions":"BASE:GPT-5.6-Sol:keep","availability_nux":{"message":"template availability"},"model_messages":{"instructions_template":"INSTRUCTIONS:GPT-5:keep"}}]}"#;
        let mut model = complete_model("deepseek-v4-flash");
        model.display_name = "DeepSeek V4 Flash".to_owned();
        let (next, _) = prepare_catalog_models(bytes, &CatalogMetadata::default(), &[model], &[])?;
        let document: Value = serde_json::from_slice(&next)?;
        let deepseek = document["models"]
            .as_array()
            .and_then(|models| {
                models
                    .iter()
                    .find(|model| model["slug"] == "deepseek-v4-flash")
            })
            .ok_or("deepseek model missing")?;
        assert!(deepseek.get("availability_nux").is_none());
        assert_eq!(deepseek["base_instructions"], "BASE:DeepSeek V4 Flash:keep");
        assert_eq!(
            deepseek["model_messages"]["instructions_template"],
            "INSTRUCTIONS:DeepSeek V4 Flash:keep"
        );
        assert_eq!(deepseek["service_tiers"], serde_json::json!([]));
        assert!(deepseek.get("default_service_tier").is_none());
        assert_eq!(deepseek["supports_reasoning_summary_parameter"], false);
        assert_eq!(deepseek["default_reasoning_summary"], "none");
        assert!(deepseek.get("apply_patch_tool_type").is_none());
        assert!(deepseek.get("additional_speed_tiers").is_none());
        Ok(())
    }

    #[test]
    fn catalog_preview_matches_the_revision_and_metadata_written_by_save()
    -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let source = root.path().join("source.json");
        let (config, _) = fixture(root.path(), Some(&source));
        let recovery = root.path().join("recovery.json");
        let current = ensure_catalog(root.path(), &config, &recovery)?;
        let model = complete_model("alpha");
        let (preview_revision, preview_metadata) = preview_catalog_models(
            root.path(),
            &config,
            &recovery,
            std::slice::from_ref(&model),
            &current.revision,
            &[],
        )?;

        let saved =
            save_catalog_models(root.path(), &config, &recovery, &[model], &current.revision)?;

        assert_eq!(saved.revision, preview_revision);
        assert_eq!(read_metadata(root.path()), preview_metadata);
        Ok(())
    }

    #[test]
    fn complete_model_validation_rejects_context_outside_bounds() {
        let mut model = complete_model("alpha");
        model.context_window = Some(300_000);
        assert!(model.validate_complete().is_err());
    }

    #[test]
    fn complete_model_validation_rejects_invalid_advanced_fields() {
        let mut model = complete_model("alpha");
        model.effective_context_window_percent = Some(101);
        assert!(model.validate_complete().is_err());

        model.effective_context_window_percent = Some(90);
        model.default_service_tier = Some("priority".to_owned());
        assert!(model.validate_complete().is_err());
    }

    #[test]
    fn complete_model_save_generates_safe_template_fields() {
        let model = complete_model("alpha").with_safe_defaults();
        assert!(model.base_instructions.is_some());
        assert_eq!(model.minimal_client_version.as_deref(), Some("0.0.0"));
        assert_eq!(model.auto_compact_token_limit, Some(112_500));
        assert_eq!(model.field_sources["baseInstructions"], "模板");
    }
}
