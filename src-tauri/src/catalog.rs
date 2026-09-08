use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::Stdio,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
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

pub(crate) fn fixed_catalog_path(home: &Path) -> PathBuf {
    home.join(".codex")
        .join("model-catalogs")
        .join("ai_cove_turbo.json")
}

fn paths_eq(left: &Path, right: &Path) -> bool {
    left.components().eq(right.components())
}

fn is_fixed_pointer(pointer: Option<&Path>, fixed_path: &Path) -> bool {
    pointer.is_some_and(|path| paths_eq(path, fixed_path))
}

fn owned_record(record: &OwnershipRecord, fixed_path: &Path, config_path: &Path) -> bool {
    paths_eq(&record.fixed_path, fixed_path) && paths_eq(&record.config_path, config_path)
}

fn rewrite_stale_fixed_pointer(
    config_path: &Path,
    pointer: Option<&Path>,
    fixed_path: &Path,
) -> Result<(), CatalogError> {
    if pointer.is_some_and(|path| {
        paths_eq(path, fixed_path) && path.as_os_str() != fixed_path.as_os_str()
    }) {
        write_catalog_pointer(config_path, Some(fixed_path))?;
    }
    Ok(())
}

pub(crate) fn save_metadata(home: &Path, metadata: &CatalogMetadata) -> Result<(), CatalogError> {
    catalog_storage::write_metadata(&fixed_catalog_path(home), metadata)
}

pub(crate) fn read_metadata(home: &Path) -> CatalogMetadata {
    catalog_storage::read_metadata(&fixed_catalog_path(home))
}

#[derive(Clone, Copy)]
struct CatalogSyncOptions<'a> {
    expected_revision: &'a str,
    restart_required: bool,
    loaded: bool,
    request_verified: bool,
    bundled_executable: Option<&'a Path>,
}

pub(crate) fn sync_catalog(
    home: &Path,
    config_path: &Path,
    recovery_path: &Path,
    expected_revision: &str,
    restart_required: bool,
    loaded: bool,
    request_verified: bool,
) -> Result<CatalogStatus, CatalogError> {
    sync_catalog_with_executable(
        home,
        config_path,
        recovery_path,
        CatalogSyncOptions {
            expected_revision,
            restart_required,
            loaded,
            request_verified,
            bundled_executable: None,
        },
    )
}

fn sync_catalog_with_executable(
    home: &Path,
    config_path: &Path,
    recovery_path: &Path,
    options: CatalogSyncOptions<'_>,
) -> Result<CatalogStatus, CatalogError> {
    let fixed_path = fixed_catalog_path(home);
    let record = read_record(recovery_path)?.ok_or(CatalogError::SourceUnavailable)?;
    if !owned_record(&record, &fixed_path, config_path) {
        return Err(CatalogError::OwnershipConflict);
    }
    let pointer = read_catalog_pointer(config_path)?;
    if !is_fixed_pointer(pointer.as_deref(), &fixed_path) {
        return Err(CatalogError::OwnershipConflict);
    }
    rewrite_stale_fixed_pointer(config_path, pointer.as_deref(), &fixed_path)?;
    let reinitialized = if fixed_path.exists() {
        read_fixed_catalog(&fixed_path)?;
        false
    } else {
        clone_root_catalog(config_path, &fixed_path, options.bundled_executable)?;
        true
    };
    let current_bytes = fs::read(&fixed_path).map_err(CatalogError::Read)?;
    if !reinitialized
        && !options.expected_revision.is_empty()
        && digest(&current_bytes) != options.expected_revision
    {
        return Err(CatalogError::ContentChanged);
    }
    let (record, catalog_changed) = sync_catalog_from_root_with_executable(
        &fixed_path,
        recovery_path,
        record,
        options.bundled_executable,
    )?;
    status_from_file(
        &fixed_path,
        &record,
        options.restart_required || catalog_changed,
        if catalog_changed {
            false
        } else {
            options.loaded
        },
        if catalog_changed {
            false
        } else {
            options.request_verified
        },
    )
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
    if !is_fixed_pointer(pointer.as_deref(), &fixed_path) {
        return Err(CatalogError::OwnershipConflict);
    }
    let bytes = fs::read(&fixed_path).map_err(CatalogError::Read)?;
    if digest(&bytes) != expected_revision {
        return Err(CatalogError::ContentChanged);
    }
    let previous_metadata = catalog_storage::read_metadata(&fixed_path);
    if let Some(slug) = protected_root_slug(&previous_metadata, removed_slugs) {
        return Err(CatalogError::ProtectedModel(slug));
    }
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
    ensure_catalog_with_executable(home, config_path, recovery_path, None)
}

fn ensure_catalog_with_executable(
    home: &Path,
    config_path: &Path,
    recovery_path: &Path,
    bundled_executable: Option<&Path>,
) -> Result<CatalogStatus, CatalogError> {
    let fixed_path = fixed_catalog_path(home);
    if let Some(record) = read_record(recovery_path)? {
        if !owned_record(&record, &fixed_path, config_path) {
            return Err(CatalogError::OwnershipConflict);
        }
        let pointer = read_catalog_pointer(config_path)?;
        if !is_fixed_pointer(pointer.as_deref(), &fixed_path) {
            return Err(CatalogError::OwnershipConflict);
        }
        rewrite_stale_fixed_pointer(config_path, pointer.as_deref(), &fixed_path)?;
        if fixed_path.exists() {
            read_fixed_catalog(&fixed_path)?;
        } else {
            clone_root_catalog(config_path, &fixed_path, bundled_executable)?;
        }
        let (record, catalog_changed) = sync_catalog_from_root_with_executable(
            &fixed_path,
            recovery_path,
            record,
            bundled_executable,
        )?;
        return status_from_file(
            &fixed_path,
            &record,
            catalog_changed,
            !catalog_changed,
            false,
        );
    }

    let current_pointer = read_catalog_pointer(config_path)?;
    let source_path = match current_pointer.as_ref() {
        Some(source) if source != &fixed_path => {
            if let Some(bytes) = read_usable_catalog(source) {
                write_atomic(&fixed_path, &bytes)?;
                Some(source.clone())
            } else {
                clone_root_catalog(config_path, &fixed_path, bundled_executable)?;
                None
            }
        }
        Some(_) => {
            if !read_fixed_catalog(&fixed_path)? {
                clone_root_catalog(config_path, &fixed_path, bundled_executable)?;
            }
            None
        }
        None => {
            if read_usable_catalog(&fixed_path).is_none() {
                clone_root_catalog(config_path, &fixed_path, bundled_executable)?;
            }
            None
        }
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
        root_fields: root_fields_from_document(&baseline_document),
        root_document: Value::Null,
        baseline_models,
        baseline_document,
    };
    write_record(recovery_path, &record)?;
    let changed_config = !is_fixed_pointer(current_pointer.as_deref(), &fixed_path);
    if !changed_config {
        rewrite_stale_fixed_pointer(config_path, current_pointer.as_deref(), &fixed_path)?;
    }
    if changed_config {
        if let Err(error) = write_catalog_pointer(config_path, Some(&fixed_path)) {
            let _ = fs::remove_file(recovery_path);
            return Err(error);
        }
    }
    status_from_file(&fixed_path, &record, changed_config, false, false)
}

fn read_usable_catalog(path: &Path) -> Option<Vec<u8>> {
    let bytes = fs::read(path).ok()?;
    let models = parse_models(&bytes).ok()?;
    (!models.is_empty()).then_some(bytes)
}

fn read_fixed_catalog(path: &Path) -> Result<bool, CatalogError> {
    match fs::read(path) {
        Ok(bytes) => {
            let models = parse_models(&bytes)?;
            if models.is_empty() {
                return Err(CatalogError::InvalidSchema("模型目录为空".to_owned()));
            }
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(CatalogError::Read(error)),
    }
}

fn clone_root_catalog(
    config_path: &Path,
    fixed_path: &Path,
    bundled_executable: Option<&Path>,
) -> Result<(), CatalogError> {
    let snapshot =
        root_snapshot(config_path, bundled_executable).ok_or(CatalogError::SourceUnavailable)?;
    if snapshot.models.is_empty() {
        return Err(CatalogError::SourceUnavailable);
    }
    let bytes = serde_json::to_vec_pretty(&snapshot.document).map_err(CatalogError::Json)?;
    write_atomic(fixed_path, &bytes)
}

pub(crate) fn reclaim_catalog(
    home: &Path,
    config_path: &Path,
    recovery_path: &Path,
) -> Result<CatalogStatus, CatalogError> {
    let fixed_path = fixed_catalog_path(home);
    let record = read_record(recovery_path)?.ok_or(CatalogError::SourceUnavailable)?;
    if !owned_record(&record, &fixed_path, config_path) {
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
    if !owned_record(&record, &fixed_path, config_path) {
        return Err(CatalogError::OwnershipConflict);
    }
    let pointer = read_catalog_pointer(config_path)?;
    if !is_fixed_pointer(pointer.as_deref(), &fixed_path) {
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
    if !is_fixed_pointer(pointer.as_deref(), &fixed_path) {
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
    if !is_fixed_pointer(pointer.as_deref(), &fixed_path) {
        return Err(CatalogError::OwnershipConflict);
    }
    let bytes = fs::read(&fixed_path).map_err(CatalogError::Read)?;
    if digest(&bytes) != expected_revision {
        return Err(CatalogError::ContentChanged);
    }
    let previous_metadata = catalog_storage::read_metadata(&fixed_path);
    if let Some(slug) = protected_root_slug(&previous_metadata, removed_slugs) {
        return Err(CatalogError::ProtectedModel(slug));
    }
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
            select_template(entries).ok_or_else(|| {
                CatalogError::InvalidSchema("新增模型缺少可用的 Codex 基准模板".to_owned())
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
                    "新增模型缺少可用的 Codex 基准模板"
                )));
            };
            let source_model = models
                .iter()
                .find(|source| source.slug == model.slug)
                .unwrap_or(model);
            let mut created = template.clone();
            if let Some(object) = created.as_object_mut() {
                if source_model
                    .base_instructions
                    .as_deref()
                    .is_none_or(|value| value.trim().is_empty())
                {
                    object.remove("base_instructions");
                }
                if let Some(messages) = object
                    .get_mut("model_messages")
                    .and_then(Value::as_object_mut)
                {
                    messages.remove("instructions_template");
                }
            }
            write_model_fields(&mut created, model);
            sanitize_new_model_template(&mut created, template, source_model);
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
    if let Some(template) = template.as_ref() {
        metadata.root_template_source_slug = template
            .get("slug")
            .and_then(Value::as_str)
            .map(str::to_owned);
        metadata.root_template_source_digest = canonical_digest(template).ok();
    }
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

fn select_template(entries: &[Value]) -> Option<Value> {
    let mut candidates = entries
        .iter()
        .filter_map(|entry| {
            entry.get("slug").and_then(Value::as_str)?;
            let valid_truncation = entry
                .get("truncation_policy")
                .and_then(Value::as_object)
                .is_some_and(|policy| {
                    matches!(
                        policy.get("mode").and_then(Value::as_str),
                        Some("tokens" | "bytes")
                    ) && policy
                        .get("limit")
                        .and_then(Value::as_u64)
                        .is_some_and(|limit| limit > 0)
                });
            let valid_shell = entry
                .get("shell_type")
                .and_then(Value::as_str)
                .is_some_and(|shell| !shell.trim().is_empty());
            let valid_verbosity = entry
                .get("support_verbosity")
                .is_some_and(Value::is_boolean);
            (valid_truncation
                && valid_shell
                && valid_verbosity
                && has_complete_prompt_fields(entry))
            .then_some(entry)
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|entry| {
        (
            !entry
                .get("is_default")
                .or_else(|| entry.get("default"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
            entry.get("visibility").and_then(Value::as_str) != Some("list"),
            !entry
                .get("supported_in_api")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            entry
                .get("priority")
                .and_then(Value::as_i64)
                .unwrap_or_default(),
            entry
                .get("slug")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        )
    });
    candidates.first().cloned().cloned()
}

fn has_complete_prompt_fields(entry: &Value) -> bool {
    let base = entry
        .get("base_instructions")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.trim().is_empty());
    let template = entry
        .get("model_messages")
        .and_then(Value::as_object)
        .and_then(|messages| messages.get("instructions_template"))
        .and_then(Value::as_str)
        .is_some_and(|value| !value.trim().is_empty());
    base && template
}

fn remove_catalog_entries(entries: &mut Vec<Value>, removed_slugs: &[String]) {
    entries.retain(|entry| {
        entry
            .get("slug")
            .and_then(Value::as_str)
            .is_none_or(|slug| !removed_slugs.iter().any(|removed| removed == slug))
    });
}

fn protected_root_slug(metadata: &CatalogMetadata, removed_slugs: &[String]) -> Option<String> {
    (metadata.root_available && metadata.root_source_type.as_deref() == Some("bundled_cli"))
        .then(|| {
            removed_slugs
                .iter()
                .find(|slug| {
                    metadata
                        .root_seen_slugs
                        .iter()
                        .any(|root_slug| root_slug == *slug)
                })
                .cloned()
        })
        .flatten()
}

fn sync_catalog_from_root_with_executable(
    fixed_path: &Path,
    recovery_path: &Path,
    mut record: OwnershipRecord,
    bundled_executable: Option<&Path>,
) -> Result<(OwnershipRecord, bool), CatalogError> {
    let read_at = now_ms_string();
    let Some(snapshot) = root_snapshot(&record.config_path, bundled_executable) else {
        let mut metadata = catalog_storage::read_metadata(fixed_path);
        metadata.root_available = false;
        metadata.root_unavailable_reason = Some("root_catalog_unavailable".to_owned());
        metadata.root_unavailable_at = Some(read_at.clone());
        metadata.root_last_read_at = Some(read_at);
        catalog_storage::write_metadata(fixed_path, &metadata)?;
        return Ok((record, false));
    };
    let mut source_document = snapshot.document;
    let source_models = snapshot.models;
    let source_digest = canonical_digest(&source_document).map_err(CatalogError::Json)?;
    let metadata_before = catalog_storage::read_metadata(fixed_path);
    let mut metadata = metadata_before.clone();
    metadata.root_last_read_at = Some(read_at);
    metadata.root_available = true;
    metadata.root_unavailable_reason = None;
    metadata.root_unavailable_at = None;
    metadata.root_source_type = Some(snapshot.source_type);
    metadata.root_codex_version = snapshot.codex_version.clone();
    metadata.root_client_version = snapshot.cli_version.clone();
    metadata.root_binary_digest = snapshot.binary_digest;
    if metadata.root_source_type.as_deref() == Some("bundled_cli") {
        let previous_root_slugs = if metadata_before.root_available
            && metadata_before.root_source_type.as_deref() == Some("bundled_cli")
        {
            metadata_before.root_seen_slugs.clone()
        } else {
            Vec::new()
        };
        let current_root_slugs = source_models
            .iter()
            .map(|model| model.slug.as_str())
            .collect::<std::collections::HashSet<_>>();
        let mut removed_slugs = metadata_before.root_removed_slugs.clone();
        for slug in previous_root_slugs {
            if !current_root_slugs.contains(slug.as_str()) && !removed_slugs.contains(&slug) {
                removed_slugs.push(slug);
            }
        }
        removed_slugs.retain(|slug| !current_root_slugs.contains(slug.as_str()));
        metadata.root_removed_slugs = removed_slugs;
    }
    let root_metadata_stable = metadata_before.root_available
        && metadata_before.root_unavailable_reason.is_none()
        && metadata_before.root_unavailable_at.is_none()
        && metadata_before.root_last_read_at.is_some()
        && metadata_before.root_last_synced_at.is_some()
        && metadata_before.root_source_digest.as_deref() == Some(source_digest.as_str())
        && metadata_before.root_source_type == metadata.root_source_type
        && metadata_before.root_codex_version == metadata.root_codex_version
        && metadata_before.root_client_version == metadata.root_client_version
        && metadata_before.root_binary_digest == metadata.root_binary_digest;
    let has_previous_root_state = !record.root_fields.is_empty()
        || record.root_document.is_object()
        || metadata_before.root_source_digest.is_some();
    if has_previous_root_state && root_metadata_stable {
        return Ok((record, false));
    }
    if record.root_fields.is_empty()
        && !record.root_document.is_object()
        && metadata_before.root_source_digest.as_deref() == Some(source_digest.as_str())
    {
        if metadata != metadata_before {
            catalog_storage::write_metadata(fixed_path, &metadata)?;
        }
        return Ok((record, false));
    }
    if let Some(models) = source_document
        .get_mut("models")
        .and_then(Value::as_array_mut)
    {
        migrate_legacy_models(models);
    }
    let current_bytes = fs::read(fixed_path).map_err(CatalogError::Read)?;
    let mut current_document: Value =
        serde_json::from_slice(&current_bytes).map_err(CatalogError::Json)?;
    if let Some(models) = current_document
        .get_mut("models")
        .and_then(Value::as_array_mut)
    {
        migrate_legacy_models(models);
    }
    let previous_document = previous_root_document(&record);
    merge_root_models(
        &mut current_document,
        &previous_document,
        &source_document,
        &mut metadata,
    )?;
    if validate_codex_document(&current_document).is_err() {
        return Ok((record, false));
    }
    let next_digest = source_digest;
    let next_seen = source_models
        .iter()
        .map(|model| model.slug.clone())
        .collect::<Vec<_>>();
    metadata.root_source_digest = Some(next_digest);
    metadata.root_seen_slugs = next_seen.clone();
    metadata.root_field_digests = root_field_digests(&source_document);
    let next_bytes = serde_json::to_vec_pretty(&current_document).map_err(CatalogError::Json)?;
    let catalog_changed = next_bytes != current_bytes;
    metadata.root_last_synced_at = Some(now_ms_string());
    let metadata_changed = metadata != metadata_before;
    let latest_bytes = fs::read(fixed_path).map_err(CatalogError::Read)?;
    if digest(&latest_bytes) != digest(&current_bytes) {
        return Err(CatalogError::ContentChanged);
    }
    let recovery_before = fs::read(recovery_path).map_err(CatalogError::Read)?;
    let mut rollback = CatalogSyncRollbackGuard::new(
        fixed_path,
        &current_bytes,
        &metadata_before,
        recovery_path,
        &recovery_before,
    );
    rollback.catalog_touched = catalog_changed;
    rollback.metadata_touched = metadata_changed;
    if catalog_changed {
        write_atomic(fixed_path, &next_bytes)?;
    }
    if metadata_changed {
        catalog_storage::write_metadata(fixed_path, &metadata)?;
    }
    let previous_record = record.clone();
    record.root_fields = root_fields_from_document(&source_document);
    record.root_document = Value::Null;
    record.root_slugs = next_seen;
    if record.root_fields != previous_record.root_fields
        || record.root_slugs != previous_record.root_slugs
    {
        rollback.recovery_touched = true;
        write_record(recovery_path, &record)?;
    }
    rollback.commit();
    Ok((record, catalog_changed))
}

struct CatalogSyncRollbackGuard {
    fixed_path: PathBuf,
    current_bytes: Vec<u8>,
    metadata_before: CatalogMetadata,
    recovery_path: PathBuf,
    recovery_before: Vec<u8>,
    catalog_touched: bool,
    metadata_touched: bool,
    recovery_touched: bool,
    committed: bool,
}

impl CatalogSyncRollbackGuard {
    fn new(
        fixed_path: &Path,
        current_bytes: &[u8],
        metadata_before: &CatalogMetadata,
        recovery_path: &Path,
        recovery_before: &[u8],
    ) -> Self {
        Self {
            fixed_path: fixed_path.to_path_buf(),
            current_bytes: current_bytes.to_owned(),
            metadata_before: metadata_before.clone(),
            recovery_path: recovery_path.to_path_buf(),
            recovery_before: recovery_before.to_owned(),
            catalog_touched: false,
            metadata_touched: false,
            recovery_touched: false,
            committed: false,
        }
    }

    const fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for CatalogSyncRollbackGuard {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        if self.catalog_touched {
            let _ = write_atomic(&self.fixed_path, &self.current_bytes);
        }
        if self.metadata_touched {
            let _ = catalog_storage::write_metadata(&self.fixed_path, &self.metadata_before);
        }
        if self.recovery_touched {
            let _ = write_atomic(&self.recovery_path, &self.recovery_before);
        }
    }
}

struct RootSnapshot {
    document: Value,
    models: Vec<CatalogModel>,
    source_type: String,
    codex_version: Option<String>,
    cli_version: Option<String>,
    binary_digest: Option<String>,
}

fn bundled_cli_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(path) = std::env::var_os("CODEX_CLI_PATH") {
        candidates.push(PathBuf::from(path));
    }
    #[cfg(windows)]
    {
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            let local = PathBuf::from(local);
            candidates.push(
                local
                    .join("Programs")
                    .join("OpenAI")
                    .join("Codex")
                    .join("bin")
                    .join("codex.exe"),
            );
            if let Ok(entries) = fs::read_dir(local.join("OpenAI").join("Codex").join("bin")) {
                let mut hashed = entries
                    .filter_map(Result::ok)
                    .map(|entry| entry.path().join("codex.exe"))
                    .filter(|path| path.is_file())
                    .collect::<Vec<_>>();
                hashed.sort();
                candidates.extend(hashed.into_iter().rev());
            }
        }
        if let Some(home) = std::env::var_os("USERPROFILE") {
            let bin = PathBuf::from(home).join(".codex").join("bin");
            candidates.push(bin.join("codex.exe"));
            candidates.push(bin.join("codex.cmd"));
        }
        if let Some(appdata) = std::env::var_os("APPDATA") {
            let npm = PathBuf::from(appdata).join("npm");
            candidates.push(npm.join("codex.cmd"));
            candidates.push(npm.join("codex.exe"));
        }
    }
    #[cfg(not(windows))]
    {
        candidates.extend([
            PathBuf::from("/opt/homebrew/bin/codex"),
            PathBuf::from("/usr/local/bin/codex"),
            PathBuf::from("/Applications/ChatGPT.app/Contents/Resources/codex"),
            PathBuf::from("/Applications/Codex.app/Contents/Resources/codex"),
        ]);
    }
    candidates.push(PathBuf::from("codex"));
    candidates
}

fn root_snapshot(config_path: &Path, bundled_executable: Option<&Path>) -> Option<RootSnapshot> {
    let candidates = bundled_executable.map_or_else(bundled_cli_candidates, |executable| {
        vec![executable.to_path_buf()]
    });
    for executable in &candidates {
        if executable.is_absolute() && !executable.is_file() {
            continue;
        }
        let Some(source_bytes) = bundled_catalog_bytes(executable, config_path.parent()) else {
            continue;
        };
        let Ok(source_document) = serde_json::from_slice::<Value>(&source_bytes) else {
            continue;
        };
        let Ok(source_models) = parse_models(&source_bytes) else {
            continue;
        };
        let catalog_version = source_document
            .get("version")
            .and_then(Value::as_str)
            .map(str::to_owned);
        return Some(RootSnapshot {
            codex_version: catalog_version,
            cli_version: codex_version(executable, config_path.parent()),
            document: source_document,
            models: source_models,
            source_type: "bundled_cli".to_owned(),
            binary_digest: fs::read(executable).ok().map(|bytes| digest(&bytes)),
        });
    }
    None
}

fn root_fields_from_document(
    document: &Value,
) -> std::collections::BTreeMap<String, std::collections::BTreeMap<String, Value>> {
    document
        .get("models")
        .and_then(Value::as_array)
        .map(|models| {
            models
                .iter()
                .filter_map(|model| {
                    let slug = model.get("slug")?.as_str()?.to_owned();
                    let fields = model
                        .as_object()?
                        .iter()
                        .filter(|(key, _)| key.as_str() != "slug")
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect();
                    Some((slug, fields))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn root_field_digests(
    document: &Value,
) -> std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>> {
    document
        .get("models")
        .and_then(Value::as_array)
        .map(|models| {
            models
                .iter()
                .filter_map(|model| {
                    let slug = model.get("slug")?.as_str()?.to_owned();
                    let fields = model.as_object()?.iter().filter_map(|(key, value)| {
                        (key != "slug")
                            .then(|| canonical_digest(value).ok())
                            .flatten()
                            .map(|digest| (key.clone(), digest))
                    });
                    Some((slug, fields.collect()))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn previous_root_document(record: &OwnershipRecord) -> Value {
    if record.root_fields.is_empty() {
        return if record.root_document.is_object() {
            record.root_document.clone()
        } else {
            record.baseline_document.clone()
        };
    }
    let mut document = record.baseline_document.clone();
    let Some(models) = document.get_mut("models").and_then(Value::as_array_mut) else {
        return document;
    };
    models.retain(|model| {
        model
            .get("slug")
            .and_then(Value::as_str)
            .is_none_or(|slug| record.root_fields.contains_key(slug))
    });
    for (slug, fields) in &record.root_fields {
        let mut model = serde_json::Map::new();
        model.extend(fields.clone());
        model.insert("slug".to_owned(), Value::String(slug.clone()));
        if let Some(existing) = models
            .iter_mut()
            .find(|entry| entry.get("slug").and_then(Value::as_str) == Some(slug.as_str()))
        {
            *existing = Value::Object(model);
        } else {
            models.push(Value::Object(model));
        }
    }
    document
}

fn now_ms_string() -> String {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or_else(
        |_| "0".to_owned(),
        |duration| duration.as_millis().to_string(),
    )
}

fn codex_version(executable: &Path, current_dir: Option<&Path>) -> Option<String> {
    let mut command = crate::process_command(executable);
    command
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(current_dir) = current_dir {
        command.current_dir(current_dir);
    }
    let output = command.output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .filter(|version| !version.is_empty())
}

fn canonical_digest(value: &Value) -> Result<String, serde_json::Error> {
    fn canonical(value: &Value) -> Value {
        match value {
            Value::Object(object) => {
                let mut entries = object.iter().collect::<Vec<_>>();
                entries.sort_by(|(left, _), (right, _)| left.cmp(right));
                let mut canonical_object = serde_json::Map::new();
                for (key, value) in entries {
                    canonical_object.insert(key.clone(), canonical(value));
                }
                Value::Object(canonical_object)
            }
            Value::Array(values) => Value::Array(values.iter().map(canonical).collect()),
            value => value.clone(),
        }
    }
    serde_json::to_vec(&canonical(value)).map(|bytes| digest(&bytes))
}

fn bundled_catalog_bytes(executable: &Path, current_dir: Option<&Path>) -> Option<Vec<u8>> {
    let mut command = crate::process_command(executable);
    command
        .args(["debug", "models", "--bundled"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(current_dir) = current_dir {
        command.current_dir(current_dir);
    }
    let mut child = command.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = thread::spawn(move || {
        let mut output = Vec::new();
        stdout.read_to_end(&mut output).ok().map(|_| output)
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return status
                    .success()
                    .then(|| reader.join().ok().flatten())
                    .flatten();
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return None;
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return None;
            }
        }
    }
}

fn merge_root_models(
    current_document: &mut Value,
    previous_document: &Value,
    next_document: &Value,
    metadata: &mut CatalogMetadata,
) -> Result<(), CatalogError> {
    let current_models = current_document
        .get_mut("models")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| CatalogError::InvalidSchema("缺少 models 数组".to_owned()))?;
    let previous_models = previous_document
        .get("models")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let next_models = next_document
        .get("models")
        .and_then(Value::as_array)
        .ok_or_else(|| CatalogError::InvalidSchema("根目录缺少 models 数组".to_owned()))?;
    let template = select_template(current_models).or_else(|| select_template(next_models));
    if let Some(template) = template.as_ref() {
        metadata.root_template_source_slug = template
            .get("slug")
            .and_then(Value::as_str)
            .map(str::to_owned);
        metadata.root_template_source_digest = canonical_digest(template).ok();
    }
    for next_model in next_models {
        let Some(slug) = next_model.get("slug").and_then(Value::as_str) else {
            continue;
        };
        let Some(next_object) = next_model.as_object() else {
            continue;
        };
        let previous_model = previous_models
            .iter()
            .find(|model| model.get("slug").and_then(Value::as_str) == Some(slug));
        let Some(current_index) = current_models
            .iter()
            .position(|model| model.get("slug").and_then(Value::as_str) == Some(slug))
        else {
            let mut added = next_model.clone();
            let complete_shape = root_model_shape_is_complete(&added);
            normalize_root_model(&mut added, template.as_ref());
            validate_root_model(&added, complete_shape)?;
            current_models.push(added);
            continue;
        };
        let Some(current_object) = current_models[current_index].as_object_mut() else {
            continue;
        };
        let previous_object = previous_model.and_then(Value::as_object);
        let mut keys = std::collections::HashSet::new();
        keys.extend(next_object.keys().cloned());
        if let Some(previous_object) = previous_object {
            keys.extend(previous_object.keys().cloned());
        }
        for key in keys {
            if key == "slug" {
                continue;
            }
            let old = previous_object.and_then(|object| object.get(&key));
            let current = current_object.get(&key);
            let next = next_object.get(&key);
            if current == next {
                set_root_field_source(metadata, slug, &key, "上游");
                clear_root_conflict(metadata, slug, &key);
                clear_user_override(metadata, slug, &key);
                continue;
            }
            if next == old {
                if current != old {
                    set_user_override(metadata, slug, &key);
                }
                continue;
            }
            if current == old {
                match next {
                    Some(value) => {
                        current_object.insert(key.clone(), value.clone());
                    }
                    None => {
                        current_object.remove(&key);
                    }
                }
                set_root_field_source(metadata, slug, &key, "上游");
                clear_user_override(metadata, slug, &key);
            } else if matches!(
                key.as_str(),
                "display_name" | "description" | "visibility" | "priority"
            ) {
                set_root_field_source(metadata, slug, &key, "冲突");
                let conflicts = metadata.conflicts.entry(slug.to_owned()).or_default();
                let marker = format!("{key}: 根目录建议未覆盖 Turbo 值");
                if !conflicts.contains(&marker) {
                    conflicts.push(marker);
                }
                set_user_override(metadata, slug, &key);
            } else {
                set_root_field_source(metadata, slug, &key, "冲突");
                let conflicts = metadata.conflicts.entry(slug.to_owned()).or_default();
                let marker = format!("{key}: 根目录更新与 Turbo 修改冲突，保留 Turbo 值");
                if !conflicts.contains(&marker) {
                    conflicts.push(marker);
                }
                set_user_override(metadata, slug, &key);
            }
        }
    }
    for model in current_models.iter_mut() {
        let complete_shape = root_model_shape_is_complete(model);
        let slug = model.get("slug").and_then(Value::as_str).map(str::to_owned);
        normalize_root_model(model, template.as_ref());
        validate_root_model(model, complete_shape)?;
        if !complete_shape {
            if let Some(slug) = slug.as_deref() {
                set_root_field_source(metadata, slug, "capabilities", "待确认");
                let conflicts = metadata.conflicts.entry(slug.to_owned()).or_default();
                let marker = "capabilities: 根目录条目能力不完整，待确认".to_owned();
                if !conflicts.contains(&marker) {
                    conflicts.push(marker);
                }
            }
        }
    }
    current_models.sort_by(|left, right| {
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
    Ok(())
}

fn normalize_root_model(model: &mut Value, template: Option<&Value>) {
    let Some(slug) = model.get("slug").and_then(Value::as_str).map(str::to_owned) else {
        return;
    };
    let original = model_from_discovery(model, slug.clone());
    let own_base = model.get("base_instructions").cloned();
    let own_messages = model
        .get("model_messages")
        .and_then(Value::as_object)
        .and_then(|messages| messages.get("instructions_template"))
        .cloned();
    let normalized = CatalogModel::with_safe_defaults(model_from_discovery(model, slug));
    write_model_fields(model, &normalized);
    if let Some(base) =
        own_base.filter(|value| value.as_str().is_some_and(|text| !text.trim().is_empty()))
    {
        if let Some(object) = model.as_object_mut() {
            object.insert("base_instructions".to_owned(), base);
        }
    } else if let Some(base) = template.and_then(|value| value.get("base_instructions")) {
        if let Some(object) = model.as_object_mut() {
            if let Some(base) = base.as_str() {
                object.insert(
                    "base_instructions".to_owned(),
                    Value::String(replace_template_identity(
                        base,
                        template.unwrap_or(&Value::Null),
                        &original,
                    )),
                );
            }
        }
    }
    let inherited_instructions = own_messages.is_none();
    if let Some(instructions) = own_messages.or_else(|| {
        template
            .and_then(|value| value.get("model_messages"))
            .and_then(Value::as_object)
            .and_then(|messages| messages.get("instructions_template"))
            .cloned()
    }) {
        let object = model.as_object_mut();
        if let Some(object) = object {
            let messages = object
                .entry("model_messages".to_owned())
                .or_insert_with(|| Value::Object(serde_json::Map::new()));
            if let Some(messages) = messages.as_object_mut() {
                let instructions = if inherited_instructions {
                    instructions.clone().as_str().map_or(instructions, |value| {
                        Value::String(replace_template_identity(
                            value,
                            template.unwrap_or(&Value::Null),
                            &original,
                        ))
                    })
                } else {
                    instructions
                };
                messages.insert("instructions_template".to_owned(), instructions);
            }
        }
    }
    if let Some(template) = template {
        sanitize_new_model_template(model, template, &original);
    }
}

fn set_root_field_source(metadata: &mut CatalogMetadata, slug: &str, key: &str, source: &str) {
    let field = match key {
        "display_name" => "displayName",
        "context_window" => "contextWindow",
        "max_context_window" => "maxContextWindow",
        "supported_reasoning_levels" => "supportedReasoningLevels",
        "default_reasoning_level" => "defaultReasoningLevel",
        "input_modalities" => "inputModalities",
        "base_instructions" => "baseInstructions",
        _ => key,
    };
    metadata
        .field_sources
        .entry(slug.to_owned())
        .or_default()
        .insert(field.to_owned(), source.to_owned());
}

fn clear_root_conflict(metadata: &mut CatalogMetadata, slug: &str, key: &str) {
    if let Some(conflicts) = metadata.conflicts.get_mut(slug) {
        conflicts.retain(|marker| !marker.starts_with(&format!("{key}:")));
        if conflicts.is_empty() {
            metadata.conflicts.remove(slug);
        }
    }
}

fn set_user_override(metadata: &mut CatalogMetadata, slug: &str, key: &str) {
    let fields = metadata.user_overrides.entry(slug.to_owned()).or_default();
    if !fields.iter().any(|field| field == key) {
        fields.push(key.to_owned());
    }
}

fn clear_user_override(metadata: &mut CatalogMetadata, slug: &str, key: &str) {
    let Some(fields) = metadata.user_overrides.get_mut(slug) else {
        return;
    };
    fields.retain(|field| field != key);
    if fields.is_empty() {
        metadata.user_overrides.remove(slug);
    }
}

fn remove_catalog_metadata(metadata: &mut CatalogMetadata, removed_slugs: &[String]) {
    for slug in removed_slugs {
        metadata.field_sources.remove(slug);
        metadata.conflicts.remove(slug);
        metadata.root_field_digests.remove(slug);
        metadata.user_overrides.remove(slug);
        metadata
            .root_removed_slugs
            .retain(|removed| removed != slug);
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
        for key in [
            "apply_patch_tool_type",
            "additional_speed_tiers",
            "supports_reasoning_summary_parameter",
            "default_reasoning_summary",
            "service_tiers",
            "default_service_tier",
            "use_responses_lite",
            "prefer_websockets",
            "supports_image_detail_original",
            "supports_search_tool",
            "supports_parallel_tool_calls",
            "tool_mode",
            "experimental_supported_tools",
            "supported_tools",
            "tools",
            "experimental_tools",
        ] {
            object.remove(key);
        }
        object.insert("service_tiers".to_owned(), serde_json::json!([]));
        object.insert(
            "supports_reasoning_summary_parameter".to_owned(),
            serde_json::Value::Bool(false),
        );
        object.insert(
            "default_reasoning_summary".to_owned(),
            serde_json::Value::String("none".to_owned()),
        );
        object.insert("use_responses_lite".to_owned(), Value::Bool(false));
        object.insert("prefer_websockets".to_owned(), Value::Bool(false));
        object.insert(
            "supports_image_detail_original".to_owned(),
            Value::Bool(false),
        );
        object.insert("supports_search_tool".to_owned(), Value::Bool(false));
        object.insert(
            "supports_parallel_tool_calls".to_owned(),
            Value::Bool(false),
        );
        object.insert(
            "experimental_supported_tools".to_owned(),
            serde_json::json!([]),
        );
    }
    if target
        .base_instructions
        .as_deref()
        .is_none_or(|value| value.trim().is_empty())
    {
        if let Some(instructions) = template.get("base_instructions").cloned() {
            if let Some(instructions) = instructions.as_str() {
                object.insert(
                    "base_instructions".to_owned(),
                    Value::String(replace_template_identity(instructions, template, target)),
                );
            }
        }
    } else if let Some(instructions) = target.base_instructions.as_deref() {
        object.insert(
            "base_instructions".to_owned(),
            Value::String(instructions.to_owned()),
        );
    }
    let template_instructions = template
        .get("model_messages")
        .and_then(Value::as_object)
        .and_then(|messages| messages.get("instructions_template"))
        .cloned();
    if let Some(instructions) = template_instructions {
        let messages = object
            .entry("model_messages".to_owned())
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
        if let Some(messages) = messages.as_object_mut() {
            let existing = messages.get("instructions_template").cloned();
            let missing = existing
                .as_ref()
                .and_then(Value::as_str)
                .is_none_or(|value| value.trim().is_empty());
            if missing {
                if let Some(instructions) = instructions.as_str() {
                    messages.insert(
                        "instructions_template".to_owned(),
                        Value::String(replace_template_identity(instructions, template, target)),
                    );
                }
            }
        }
    }
}

fn replace_template_identity(text: &str, template: &Value, target: &CatalogModel) -> String {
    let target_name = if target.display_name.trim().is_empty() {
        target.slug.trim()
    } else {
        target.display_name.trim()
    };
    let mut sources = Vec::new();
    for key in ["display_name", "slug"] {
        if let Some(value) = template.get(key).and_then(Value::as_str) {
            sources.extend(identity_aliases(value));
        }
    }
    if sources.is_empty() {
        return text.to_owned();
    }
    let mut result = text.to_owned();
    sources.sort_by_key(|value| std::cmp::Reverse(value.len()));
    sources.dedup();
    for source in sources {
        if !source.is_empty() {
            result = result.replace(&source, target_name);
        }
    }
    result
}

fn identity_aliases(raw: &str) -> Vec<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Vec::new();
    }
    let mut aliases = vec![raw.to_owned(), raw.replace(['-', '_'], " ")];
    let parts = raw
        .split(['-', '_', ' '])
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    for n in 2..=parts.len() {
        aliases.push(parts[..n].join("-"));
        aliases.push(parts[..n].join(" "));
        aliases.push(format!(
            "{}-{}",
            parts[0].to_ascii_uppercase(),
            parts[1..n].join("-")
        ));
        if let Some((major, rest)) = parts[1].split_once('.')
            && !rest.is_empty()
            && major.chars().all(|ch| ch.is_ascii_digit())
        {
            aliases.push(format!("{}-{major}", parts[0].to_ascii_uppercase()));
        }
    }
    aliases
}

fn validate_root_model(model: &Value, complete_shape: bool) -> Result<(), CatalogError> {
    let Some(slug) = model.get("slug").and_then(Value::as_str) else {
        return Ok(());
    };
    let parsed = model_from_discovery(model, slug.to_owned());
    if complete_shape {
        CatalogModel::with_safe_defaults(parsed)
            .validate_complete()
            .map_err(CatalogError::InvalidSchema)?;
    }
    Ok(())
}

fn root_model_shape_is_complete(model: &Value) -> bool {
    model.get("context_window").is_some()
        || model.get("max_context_window").is_some()
        || model
            .get("supported_reasoning_levels")
            .and_then(Value::as_array)
            .is_some_and(|levels| !levels.is_empty())
        || model.get("default_reasoning_level").is_some()
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
    if !is_fixed_pointer(pointer.as_deref(), &fixed_path) {
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
    if is_fixed_pointer(pointer.as_deref(), &fixed_path) {
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
    let metadata_path = fixed_path.with_file_name("ai_cove_turbo.metadata.json");
    match fs::remove_file(metadata_path) {
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

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    use tempfile::tempdir;

    use super::*;

    fn toml_basic_string(value: &Path) -> String {
        format!(
            "\"{}\"",
            value
                .to_string_lossy()
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
        )
    }

    #[test]
    fn toml_basic_string_escapes_windows_paths() {
        assert_eq!(
            toml_basic_string(Path::new(r"C:\Users\runner\source.json")),
            r#""C:\\Users\\runner\\source.json""#
        );
    }

    #[test]
    fn mixed_separator_paths_are_the_same_catalog_file() {
        let mixed = PathBuf::from("home").join(".codex/model-catalogs/ai_cove_turbo.json");
        let native = PathBuf::from("home")
            .join(".codex")
            .join("model-catalogs")
            .join("ai_cove_turbo.json");
        assert!(paths_eq(&mixed, &native));
        assert_eq!(fixed_catalog_path(Path::new("home")), native);
        assert!(is_fixed_pointer(Some(mixed.as_path()), &native));
    }

    #[cfg(not(windows))]
    #[test]
    fn bundled_cli_candidates_include_macos_app_cli() {
        let candidates = bundled_cli_candidates();
        assert!(
            candidates.iter().any(|path| {
                path == Path::new("/Applications/Codex.app/Contents/Resources/codex")
            })
        );
        assert_eq!(
            candidates.last().map(PathBuf::as_path),
            Some(Path::new("codex"))
        );
    }

    fn fixture(root: &Path, pointer: Option<&Path>) -> (PathBuf, PathBuf) {
        let config = root.join("config.toml");
        let source = root.join("source.json");
        fs::write(
            &config,
            pointer.map_or_else(
                || "model_provider = \"custom\"\n".to_owned(),
                |path| {
                    format!(
                        "model_provider = \"custom\"\nmodel_catalog_json = {}\n",
                        toml_basic_string(path)
                    )
                },
            ),
        )
        .expect("config fixture");
        fs::write(
            &source,
            r#"{"models":[{"slug":"alpha","display_name":"Alpha","description":"a","visibility":"list","priority":2,"unknown":"kept","truncation_policy":{"mode":"bytes","limit":10000},"shell_type":"shell_command","support_verbosity":true},{"slug":"beta","display_name":"Beta","description":"b","visibility":"hide","priority":1,"truncation_policy":{"mode":"bytes","limit":10000},"shell_type":"shell_command","support_verbosity":true},{"slug":"gpt-5.6-sol","display_name":"GPT-5.6-Sol","description":"Codex template","visibility":"list","priority":0,"template_only":"kept","truncation_policy":{"mode":"tokens","limit":10000},"shell_type":"shell_command","support_verbosity":true,"base_instructions":"GPT-5.6-Sol base","model_messages":{"instructions_template":"GPT-5.6-Sol instructions"}}]}"#,
        )
        .expect("catalog fixture");
        (config, source)
    }

    #[cfg(unix)]
    fn bundled_executable(root: &Path, catalog: &str) -> Result<PathBuf, Box<dyn Error>> {
        let root_catalog = root.join("root-catalog.json");
        fs::write(&root_catalog, catalog)?;
        let executable = root.join("codex");
        fs::write(
            &executable,
            format!(
                "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then printf '%s' codex-test; else cat \"{}\"; fi\n",
                root_catalog.display()
            ),
        )?;
        let mut permissions = fs::metadata(&executable)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&executable, permissions)?;
        Ok(executable)
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

    #[cfg(unix)]
    #[test]
    fn first_launch_clones_bundled_root_catalog_into_fixed_path() -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let (config, _) = fixture(root.path(), None);
        let executable = bundled_executable(
            root.path(),
            r#"{"models":[{"slug":"root-alpha","display_name":"Root Alpha","visibility":"list","priority":1}]}"#,
        )?;

        let recovery = root.path().join("recovery.json");
        let status =
            ensure_catalog_with_executable(root.path(), &config, &recovery, Some(&executable))?;

        assert_eq!(
            status.models.first().map(|model| model.slug.as_str()),
            Some("root-alpha")
        );
        assert_eq!(status.source_path, None);
        assert_eq!(
            read_catalog_pointer(&config)?.as_deref(),
            Some(fixed_catalog_path(root.path()).as_path())
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&fs::read(fixed_catalog_path(root.path()))?)?,
            serde_json::json!({
                "models": [{
                    "slug": "root-alpha",
                    "display_name": "Root Alpha",
                    "visibility": "list",
                    "priority": 1
                }]
            })
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn fixed_pointer_to_missing_catalog_falls_back_to_bundled_root() -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let fixed = fixed_catalog_path(root.path());
        let (config, _) = fixture(root.path(), Some(&fixed));
        let executable = bundled_executable(
            root.path(),
            r#"{"models":[{"slug":"root-alpha","display_name":"Root Alpha","visibility":"list","priority":1}]}"#,
        )?;
        let recovery = root.path().join("recovery.json");

        let status =
            ensure_catalog_with_executable(root.path(), &config, &recovery, Some(&executable))?;

        assert_eq!(
            status.models.first().map(|model| model.slug.as_str()),
            Some("root-alpha")
        );
        assert!(fixed.exists());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn missing_fixed_catalog_with_existing_recovery_is_reinitialized() -> Result<(), Box<dyn Error>>
    {
        let root = tempdir()?;
        let source = root.path().join("source.json");
        let (config, _) = fixture(root.path(), Some(&source));
        let recovery = root.path().join("recovery.json");
        ensure_catalog(root.path(), &config, &recovery)?;
        let fixed = fixed_catalog_path(root.path());
        fs::remove_file(&fixed)?;
        let executable = bundled_executable(
            root.path(),
            r#"{"models":[{"slug":"root-alpha","display_name":"Root Alpha","visibility":"list","priority":1}]}"#,
        )?;

        let status =
            ensure_catalog_with_executable(root.path(), &config, &recovery, Some(&executable))?;

        assert_eq!(
            status.models.first().map(|model| model.slug.as_str()),
            Some("root-alpha")
        );
        assert!(fixed.exists());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn status_sync_reinitializes_missing_fixed_catalog() -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let source = root.path().join("source.json");
        let (config, _) = fixture(root.path(), Some(&source));
        let recovery = root.path().join("recovery.json");
        let initial = ensure_catalog(root.path(), &config, &recovery)?;
        let fixed = fixed_catalog_path(root.path());
        fs::remove_file(&fixed)?;
        let executable = bundled_executable(
            root.path(),
            r#"{"models":[{"slug":"root-alpha","display_name":"Root Alpha","visibility":"list","priority":1}]}"#,
        )?;

        let status = sync_catalog_with_executable(
            root.path(),
            &config,
            &recovery,
            CatalogSyncOptions {
                expected_revision: &initial.revision,
                restart_required: false,
                loaded: false,
                request_verified: false,
                bundled_executable: Some(&executable),
            },
        )?;

        assert_eq!(
            status.models.first().map(|model| model.slug.as_str()),
            Some("root-alpha")
        );
        assert!(fixed.exists());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn fixed_pointer_to_corrupt_catalog_reports_error_without_repair() -> Result<(), Box<dyn Error>>
    {
        let root = tempdir()?;
        let fixed = fixed_catalog_path(root.path());
        let (config, _) = fixture(root.path(), Some(&fixed));
        fs::create_dir_all(fixed.parent().expect("catalog parent"))?;
        fs::write(&fixed, b"not-json")?;
        let executable = bundled_executable(
            root.path(),
            r#"{"models":[{"slug":"root-alpha","display_name":"Root Alpha","visibility":"list","priority":1}]}"#,
        )?;
        let recovery = root.path().join("recovery.json");

        let result =
            ensure_catalog_with_executable(root.path(), &config, &recovery, Some(&executable));

        assert!(matches!(result, Err(CatalogError::Json(_))));
        assert_eq!(fs::read(&fixed)?, b"not-json");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn external_pointer_to_unavailable_catalog_falls_back_to_bundled_root()
    -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let missing = root.path().join("missing.json");
        let (config, _) = fixture(root.path(), Some(&missing));
        let executable = bundled_executable(
            root.path(),
            r#"{"models":[{"slug":"root-alpha","display_name":"Root Alpha","visibility":"list","priority":1}]}"#,
        )?;
        let recovery = root.path().join("recovery.json");

        let status =
            ensure_catalog_with_executable(root.path(), &config, &recovery, Some(&executable))?;

        assert_eq!(
            status.models.first().map(|model| model.slug.as_str()),
            Some("root-alpha")
        );
        assert!(fixed_catalog_path(root.path()).exists());
        Ok(())
    }

    #[test]
    fn available_fixed_catalog_pointer_is_reused() -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let fixed = fixed_catalog_path(root.path());
        let (config, _) = fixture(root.path(), Some(&fixed));
        fs::create_dir_all(fixed.parent().expect("catalog parent"))?;
        fs::write(
            &fixed,
            r#"{"models":[{"slug":"fixed-alpha","display_name":"Fixed Alpha","visibility":"list","priority":1}]}"#,
        )?;
        let status = ensure_catalog(root.path(), &config, &root.path().join("recovery.json"))?;
        assert_eq!(
            status.models.first().map(|model| model.slug.as_str()),
            Some("fixed-alpha")
        );
        assert!(fs::read_to_string(config)?.contains("model_catalog_json"));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn empty_fixed_catalog_is_reinitialized_from_bundled_root() -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let (config, _) = fixture(root.path(), None);
        let fixed = fixed_catalog_path(root.path());
        fs::create_dir_all(fixed.parent().expect("catalog parent"))?;
        fs::write(&fixed, r#"{"models":[]}"#)?;
        let executable = bundled_executable(
            root.path(),
            r#"{"models":[{"slug":"root-alpha","display_name":"Root Alpha","visibility":"list","priority":1}]}"#,
        )?;

        let status = ensure_catalog_with_executable(
            root.path(),
            &config,
            &root.path().join("recovery.json"),
            Some(&executable),
        )?;

        assert_eq!(
            status.models.first().map(|model| model.slug.as_str()),
            Some("root-alpha")
        );
        Ok(())
    }

    #[test]
    fn relative_catalog_pointer_resolves_from_codex_config_directory() -> Result<(), Box<dyn Error>>
    {
        let root = tempdir()?;
        let config_dir = root.path().join("home/.codex");
        fs::create_dir_all(&config_dir)?;
        let source = config_dir.join("models.json");
        fs::write(
            &source,
            r#"{"models":[{"slug":"relative-alpha","display_name":"Relative Alpha","visibility":"list","priority":1}]}"#,
        )?;
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
        assert_eq!(
            status.models.first().map(|model| model.slug.as_str()),
            Some("relative-alpha")
        );
        Ok(())
    }

    #[test]
    fn legacy_source_model_is_not_protected_without_bundled_root() -> Result<(), Box<dyn Error>> {
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
            &["gpt-5.6-sol".to_owned()],
        );

        assert!(result.is_ok());
        assert!(!fs::read_to_string(fixed_catalog_path(root.path()))?.contains("gpt-5.6-sol"));
        Ok(())
    }

    #[test]
    fn legacy_source_models_are_not_treated_as_codex_presets() -> Result<(), Box<dyn Error>> {
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

        assert!(result.is_ok());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn catalog_status_does_not_treat_legacy_source_as_codex_root() -> Result<(), Box<dyn Error>> {
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
        assert!(!alpha.root_presence);
        assert!(!alpha.root_missing);
        assert!(!gamma.root_presence);
        assert!(!gamma.root_missing);

        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn bundled_root_marks_only_previously_seen_model_as_removed() -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let source = root.path().join("source.json");
        let (config, _) = fixture(root.path(), Some(&source));
        let recovery = root.path().join("recovery.json");
        let initial = ensure_catalog(root.path(), &config, &recovery)?;
        save_catalog_models(
            root.path(),
            &config,
            &recovery,
            &[
                complete_model("gemini-3.8-flash"),
                complete_model("grok-4.6"),
            ],
            &initial.revision,
        )?;
        let bundled_v1 = root.path().join("bundled-v1.json");
        fs::write(
            &bundled_v1,
            br#"{"models":[{"slug":"grok-4.6","display_name":"grok-4.6","description":"Codex root","visibility":"list","priority":4,"truncation_policy":{"mode":"tokens","limit":10000},"shell_type":"shell_command","support_verbosity":true}]}"#,
        )?;
        let executable = root.path().join("codex-v1");
        fs::write(
            &executable,
            format!("#!/bin/sh\ncat \"{}\"\n", bundled_v1.display()),
        )?;
        let mut permissions = fs::metadata(&executable)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&executable, permissions)?;

        let (record, _) = sync_catalog_from_root_with_executable(
            &fixed_catalog_path(root.path()),
            &recovery,
            read_record(&recovery)?.ok_or("recovery missing")?,
            Some(&executable),
        )?;
        assert_eq!(
            catalog_storage::read_metadata(&fixed_catalog_path(root.path())).root_seen_slugs,
            vec!["grok-4.6".to_owned()]
        );
        let status_after_first_root = status_from_file(
            &fixed_catalog_path(root.path()),
            &record,
            true,
            false,
            false,
        )?;
        let grok_after_first_root = status_after_first_root
            .models
            .iter()
            .find(|model| model.slug == "grok-4.6")
            .ok_or("grok missing")?;
        assert!(grok_after_first_root.root_presence);
        assert!(!grok_after_first_root.root_missing);
        assert!(matches!(
            save_catalog_models_with_removals(
                root.path(),
                &config,
                &recovery,
                &[],
                &status_after_first_root.revision,
                &["grok-4.6".to_owned()],
            ),
            Err(CatalogError::ProtectedModel(slug)) if slug == "grok-4.6"
        ));
        let bundled_v2 = root.path().join("bundled-v2.json");
        fs::write(
            &bundled_v2,
            br#"{"models":[{"slug":"gpt-5.6-sol","display_name":"GPT-5.6-Sol","description":"Codex root","visibility":"list","priority":1,"truncation_policy":{"mode":"tokens","limit":10000},"shell_type":"shell_command","support_verbosity":true}]}"#,
        )?;
        fs::write(
            &executable,
            format!("#!/bin/sh\ncat \"{}\"\n", bundled_v2.display()),
        )?;
        let (record, _) = sync_catalog_from_root_with_executable(
            &fixed_catalog_path(root.path()),
            &recovery,
            record,
            Some(&executable),
        )?;
        let status = status_from_file(
            &fixed_catalog_path(root.path()),
            &record,
            true,
            false,
            false,
        )?;
        let gemini = status
            .models
            .iter()
            .find(|model| model.slug == "gemini-3.8-flash")
            .ok_or("gemini missing")?;
        let grok = status
            .models
            .iter()
            .find(|model| model.slug == "grok-4.6")
            .ok_or("grok missing")?;
        assert!(!gemini.root_presence);
        assert!(!gemini.root_missing);
        assert!(!grok.root_presence);
        assert!(grok.root_missing, "metadata={:?}", status.metadata);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn unavailable_bundled_root_does_not_protect_stale_model() -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let source = root.path().join("source.json");
        let (config, _) = fixture(root.path(), Some(&source));
        let recovery = root.path().join("recovery.json");
        let initial = ensure_catalog(root.path(), &config, &recovery)?;
        save_catalog_models(
            root.path(),
            &config,
            &recovery,
            &[complete_model("grok-4.6")],
            &initial.revision,
        )?;
        fs::remove_file(&source)?;

        let executable = root.path().join("codex-valid");
        fs::write(&executable, "#!/bin/sh\nprintf '%s' '{\"models\":[]}'\n")?;
        let mut permissions = fs::metadata(&executable)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&executable, permissions)?;
        let (record, _) = sync_catalog_from_root_with_executable(
            &fixed_catalog_path(root.path()),
            &recovery,
            read_record(&recovery)?.ok_or("recovery missing")?,
            Some(&executable),
        )?;

        let invalid = root.path().join("codex-invalid");
        fs::write(&invalid, "#!/bin/sh\nprintf '%s' invalid\n")?;
        let mut permissions = fs::metadata(&invalid)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&invalid, permissions)?;
        let (record, _) = sync_catalog_from_root_with_executable(
            &fixed_catalog_path(root.path()),
            &recovery,
            record,
            Some(&invalid),
        )?;
        let status = status_from_file(
            &fixed_catalog_path(root.path()),
            &record,
            false,
            false,
            false,
        )?;
        let grok = status
            .models
            .iter()
            .find(|model| model.slug == "grok-4.6")
            .ok_or("grok missing")?;
        assert!(!grok.root_presence);
        assert!(!grok.root_missing);
        assert!(
            save_catalog_models_with_removals(
                root.path(),
                &config,
                &recovery,
                &[],
                &status.revision,
                &["grok-4.6".to_owned()],
            )
            .is_ok()
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn root_sync_adopts_untouched_fields_and_preserves_user_overrides() -> Result<(), Box<dyn Error>>
    {
        let root = tempdir()?;
        let source = root.path().join("source.json");
        let (config, _) = fixture(root.path(), Some(&source));
        let recovery = root.path().join("recovery.json");
        let initial = ensure_catalog(root.path(), &config, &recovery)?;
        let mut alpha = complete_model("alpha");
        alpha.description = "user override".to_owned();
        let mut beta = complete_model("beta");
        beta.description = "b".to_owned();
        let saved = save_catalog_models(
            root.path(),
            &config,
            &recovery,
            &[alpha, beta],
            &initial.revision,
        )?;

        let mut root_document: Value = serde_json::from_slice(&fs::read(&source)?)?;
        root_document["models"][0]["description"] = Value::String("root update".to_owned());
        root_document["models"][1]["description"] = Value::String("root beta update".to_owned());
        let root_bytes = serde_json::to_vec_pretty(&root_document)?;
        fs::write(&source, &root_bytes)?;

        let bundled = root.path().join("codex-bundled");
        fs::write(
            &bundled,
            format!("#!/bin/sh\ncat \"{}\"\n", source.display()),
        )?;
        let mut permissions = fs::metadata(&bundled)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&bundled, permissions)?;
        let (record, _) = sync_catalog_from_root_with_executable(
            &fixed_catalog_path(root.path()),
            &recovery,
            read_record(&recovery)?.ok_or("recovery missing")?,
            Some(&bundled),
        )?;
        let synced = status_from_file(
            &fixed_catalog_path(root.path()),
            &record,
            true,
            false,
            false,
        )?;
        let synced_alpha = synced
            .models
            .iter()
            .find(|model| model.slug == "alpha")
            .ok_or("alpha missing")?;
        let synced_beta = synced
            .models
            .iter()
            .find(|model| model.slug == "beta")
            .ok_or("beta missing")?;
        assert_eq!(synced_alpha.description, "user override");
        assert_eq!(synced_beta.description, "root beta update");
        assert!(
            synced_alpha
                .conflicts
                .iter()
                .any(|value| value.contains("description"))
        );
        assert!(
            synced
                .metadata
                .user_overrides
                .get("alpha")
                .is_some_and(|fields| fields.iter().any(|field| field == "description"))
        );
        assert_eq!(
            synced
                .metadata
                .root_field_digests
                .get("alpha")
                .and_then(|fields| fields.get("description")),
            Some(&canonical_digest(
                &root_document["models"][0]["description"]
            )?),
        );
        assert_eq!(
            synced.metadata.root_source_digest.as_deref(),
            Some(canonical_digest(&root_document)?.as_str())
        );
        assert!(
            synced
                .metadata
                .root_seen_slugs
                .iter()
                .any(|slug| slug == "beta")
        );
        assert!(saved.revision != synced.revision);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn root_sync_keeps_incomplete_added_model_pending() -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let source = root.path().join("source.json");
        let (config, _) = fixture(root.path(), Some(&source));
        let recovery = root.path().join("recovery.json");
        ensure_catalog(root.path(), &config, &recovery)?;

        let mut root_document: Value = serde_json::from_slice(&fs::read(&source)?)?;
        root_document["models"]
            .as_array_mut()
            .expect("models array")
            .push(serde_json::json!({
                "slug": "remote-basic",
                "display_name": "Remote Basic",
                "description": "能力待确认",
                "visibility": "list",
                "priority": 3,
                "truncation_policy": {"mode": "tokens", "limit": 10000},
                "shell_type": "shell_command",
                "support_verbosity": true
            }));
        fs::write(&source, serde_json::to_vec_pretty(&root_document)?)?;

        let bundled = root.path().join("codex-bundled");
        fs::write(
            &bundled,
            format!("#!/bin/sh\ncat \"{}\"\n", source.display()),
        )?;
        let mut permissions = fs::metadata(&bundled)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&bundled, permissions)?;
        let (record, _) = sync_catalog_from_root_with_executable(
            &fixed_catalog_path(root.path()),
            &recovery,
            read_record(&recovery)?.ok_or("recovery missing")?,
            Some(&bundled),
        )?;
        let refreshed = status_from_file(
            &fixed_catalog_path(root.path()),
            &record,
            true,
            false,
            false,
        )?;
        let pending = refreshed
            .models
            .iter()
            .find(|model| model.slug == "remote-basic")
            .ok_or("incomplete root model missing")?;
        assert_eq!(
            pending
                .field_sources
                .get("capabilities")
                .map(String::as_str),
            Some("待确认")
        );
        assert!(
            pending
                .conflicts
                .iter()
                .any(|value| value == "capabilities: 根目录条目能力不完整，待确认")
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn root_sync_falls_back_to_bundled_catalog_and_skips_unchanged_digest()
    -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let source = root.path().join("source.json");
        let (config, _) = fixture(root.path(), Some(&source));
        let recovery = root.path().join("recovery.json");
        ensure_catalog(root.path(), &config, &recovery)?;
        let record = read_record(&recovery)?.ok_or("recovery missing")?;
        fs::remove_file(&source)?;

        let bundled = root.path().join("bundled.json");
        let bundled_bytes = br#"{"models":[{"slug":"alpha","display_name":"Alpha","description":"bundled update","visibility":"list","priority":2,"truncation_policy":{"mode":"bytes","limit":10000},"shell_type":"shell_command","support_verbosity":true},{"slug":"beta","display_name":"Beta","description":"b","visibility":"hide","priority":1,"truncation_policy":{"mode":"bytes","limit":10000},"shell_type":"shell_command","support_verbosity":true},{"slug":"gpt-5.6-sol","display_name":"GPT-5.6-Sol","description":"Codex template","visibility":"list","priority":0,"truncation_policy":{"mode":"tokens","limit":10000},"shell_type":"shell_command","support_verbosity":true}]}"#;
        fs::write(&bundled, bundled_bytes)?;
        let executable = root.path().join("codex-fake");
        fs::write(
            &executable,
            format!("#!/bin/sh\ncat \"{}\"\n", bundled.display()),
        )?;
        let mut permissions = fs::metadata(&executable)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&executable, permissions)?;

        let (synced_record, changed) = sync_catalog_from_root_with_executable(
            &fixed_catalog_path(root.path()),
            &recovery,
            record,
            Some(&executable),
        )?;
        assert!(changed);
        let synced = status_from_file(
            &fixed_catalog_path(root.path()),
            &synced_record,
            true,
            false,
            false,
        )?;
        assert_eq!(
            synced
                .models
                .iter()
                .find(|model| model.slug == "alpha")
                .map(|model| model.description.as_str()),
            Some("bundled update")
        );
        assert_eq!(
            synced.metadata.root_source_digest.as_deref(),
            Some(canonical_digest(&serde_json::from_slice::<Value>(bundled_bytes)?)?.as_str())
        );
        let metadata_before_repeat =
            catalog_storage::read_metadata(&fixed_catalog_path(root.path()));
        let (_, changed_again) = sync_catalog_from_root_with_executable(
            &fixed_catalog_path(root.path()),
            &recovery,
            synced_record,
            Some(&executable),
        )?;
        assert!(!changed_again);
        assert_eq!(
            catalog_storage::read_metadata(&fixed_catalog_path(root.path())),
            metadata_before_repeat
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn bundled_catalog_output_is_rejected_when_not_valid_json() -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let source = root.path().join("source.json");
        let (config, _) = fixture(root.path(), Some(&source));
        let recovery = root.path().join("recovery.json");
        ensure_catalog(root.path(), &config, &recovery)?;
        let record = read_record(&recovery)?.ok_or("recovery missing")?;
        fs::remove_file(&source)?;
        let executable = root.path().join("codex-invalid");
        fs::write(&executable, "#!/bin/sh\nprintf '%s' invalid\n")?;
        let mut permissions = fs::metadata(&executable)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&executable, permissions)?;

        assert!(root_snapshot(&record.config_path, Some(&executable)).is_none());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn bundled_catalog_drains_large_stdout_before_waiting_for_exit() -> Result<(), Box<dyn Error>> {
        let root = tempdir()?;
        let executable = root.path().join("codex-large-output");
        fs::write(
            &executable,
            "#!/bin/sh\ndd if=/dev/zero bs=1024 count=128 2>/dev/null\n",
        )?;
        let mut permissions = fs::metadata(&executable)?.permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&executable, permissions)?;

        let output = bundled_catalog_bytes(&executable, None).ok_or("large output failed")?;
        assert_eq!(output.len(), 131_072);
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
                "model_provider = \"custom\"\nmodel_catalog_json = {}\n",
                toml_basic_string(&source)
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
        let bytes = br#"{"models":[{"slug":"gpt-5.6-sol","display_name":"Sol","description":"template","visibility":"list","priority":1,"context_window":125000,"max_context_window":250000,"supported_reasoning_levels":[{"effort":"low"}],"default_reasoning_level":"low","truncation_policy":{"mode":"tokens","limit":10000},"shell_type":"shell_command","support_verbosity":true,"template_only":"kept","base_instructions":"Sol base","model_messages":{"instructions_template":"Sol instructions"}}]}"#;
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
    fn new_model_replaces_gpt_6_template_identity() -> Result<(), Box<dyn Error>> {
        let bytes = br#"{"models":[{"slug":"gpt-6-astra","display_name":"GPT-6-Astra","description":"template","visibility":"list","priority":1,"context_window":125000,"max_context_window":250000,"supported_reasoning_levels":[{"effort":"low"}],"default_reasoning_level":"low","truncation_policy":{"mode":"tokens","limit":10000},"shell_type":"shell_command","support_verbosity":true,"base_instructions":"You are Codex, an agent based on GPT-6.","model_messages":{"instructions_template":"Follow GPT-6 instructions"}}]}"#;
        let mut model = complete_model("glm-4.6");
        model.display_name = "GLM-4.6".to_owned();
        let (next, _) = prepare_catalog_models(bytes, &CatalogMetadata::default(), &[model], &[])?;
        let document: Value = serde_json::from_slice(&next)?;
        let glm = document["models"]
            .as_array()
            .and_then(|models| models.iter().find(|model| model["slug"] == "glm-4.6"))
            .ok_or("glm model missing")?;
        assert_eq!(
            glm["base_instructions"],
            "You are Codex, an agent based on GLM-4.6."
        );
        assert_eq!(
            glm["model_messages"]["instructions_template"],
            "Follow GLM-4.6 instructions"
        );
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
