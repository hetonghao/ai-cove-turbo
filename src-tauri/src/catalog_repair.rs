use std::{
    fs,
    path::{Path, PathBuf},
};

use serde_json::Value;

use super::{
    CatalogError, CatalogModel, CatalogStatus, OwnershipRecord, catalog_storage,
    clone_root_catalog, digest, ensure_catalog_with_executable, fixed_catalog_path,
    is_fixed_pointer, new_ownership_record, owned_record, parse_models, paths_eq, read_catalog,
    read_catalog_pointer, read_usable_catalog, root_snapshot, valid_truncation_policy,
    validate_codex_document, write_atomic, write_model_fields, write_record,
};

pub(crate) fn failed_status(mut status: CatalogStatus, error: &CatalogError) -> CatalogStatus {
    let path = Path::new(&status.path);
    if status.models.is_empty() {
        if let Some(models) = read_usable_catalog(path)
            .or_else(|| read_usable_catalog(&last_good_path(path)))
            .and_then(|bytes| parse_models(&bytes).ok())
        {
            status.models = models;
        }
        status.metadata = catalog_storage::read_metadata(path);
    }
    status.state = if matches!(error, CatalogError::OwnershipConflict) {
        "conflict".to_owned()
    } else {
        "error".to_owned()
    };
    status.problem = Some(error.problem());
    status.loaded = false;
    status.restart_required = false;
    status.request_verified = false;
    status
}

fn last_good_path(fixed_path: &Path) -> PathBuf {
    fixed_path.with_file_name("ai_cove_turbo.last-good.json")
}

pub(super) fn restore_missing_catalog(
    fixed_path: &Path,
    pointer: Option<&Path>,
    bundled_executable: Option<&Path>,
    record: &OwnershipRecord,
) -> Result<(), CatalogError> {
    let source = pointer
        .filter(|path| !paths_eq(path, fixed_path))
        .or_else(|| record.original_model_catalog_json.as_deref().map(Path::new));
    if let Some(bytes) = read_repair_source(fixed_path, source)
        .or_else(|| record_baseline(&record.baseline_document))
    {
        write_atomic(fixed_path, &prepare_repair_document(bytes)?)
    } else {
        clone_root_catalog(&record.config_path, fixed_path, bundled_executable)
    }
}

fn read_repair_source(fixed_path: &Path, source: Option<&Path>) -> Option<Vec<u8>> {
    read_usable_catalog(&last_good_path(fixed_path))
        .or_else(|| source.and_then(read_usable_catalog))
}

fn record_baseline(document: &Value) -> Option<Vec<u8>> {
    let bytes = serde_json::to_vec_pretty(document).ok()?;
    parse_models(&bytes)
        .is_ok_and(|models| !models.is_empty())
        .then_some(bytes)
}

fn backup_damaged(path: &Path, bytes: &[u8]) -> Result<(), CatalogError> {
    let backup = path.with_extension(format!("json.damaged-{}", digest(bytes)));
    if fs::read(&backup).is_ok_and(|saved| saved == bytes) {
        return Ok(());
    }
    write_atomic(&backup, bytes)
}

pub(crate) fn quarantine_damaged_journal(path: &Path, bytes: &[u8]) -> Result<(), CatalogError> {
    backup_damaged(path, bytes)?;
    if fs::read(path).map_err(CatalogError::Read)? != bytes {
        return Err(CatalogError::ContentChanged);
    }
    fs::remove_file(path).map_err(CatalogError::Write)
}

pub(crate) fn remember_good(status: &CatalogStatus) -> Result<(), CatalogError> {
    let fixed_path = Path::new(&status.path);
    let bytes = fs::read(fixed_path).map_err(CatalogError::Read)?;
    if digest(&bytes) != status.revision {
        return Err(CatalogError::ContentChanged);
    }
    let last_good = last_good_path(fixed_path);
    if fs::read(&last_good).is_ok_and(|previous| previous == bytes) {
        return Ok(());
    }
    write_atomic(&last_good, &bytes)
}

pub(crate) fn repair_catalog(
    home: &Path,
    config_path: &Path,
    recovery_path: &Path,
    fallback_models: &[CatalogModel],
) -> Result<CatalogStatus, CatalogError> {
    repair_catalog_with_executable(home, config_path, recovery_path, fallback_models, None)
}

pub(super) fn repair_catalog_with_executable(
    home: &Path,
    config_path: &Path,
    recovery_path: &Path,
    fallback_models: &[CatalogModel],
    bundled_executable: Option<&Path>,
) -> Result<CatalogStatus, CatalogError> {
    let fixed_path = fixed_catalog_path(home);
    let pointer = read_catalog_pointer(config_path)?;
    let points_to_turbo = is_fixed_pointer(pointer.as_deref(), &fixed_path);
    let recovery_bytes = catalog_storage::read_optional(recovery_path)?;
    let record = recovery_bytes
        .as_deref()
        .and_then(|bytes| serde_json::from_slice::<OwnershipRecord>(bytes).ok());
    let damaged_recovery = recovery_bytes.is_some() && record.is_none();
    if record.as_ref().is_some_and(|record| {
        !owned_record(record, &fixed_path, config_path) || (!record.released && !points_to_turbo)
    }) || (damaged_recovery && pointer.is_some() && !points_to_turbo)
    {
        return Err(CatalogError::OwnershipConflict);
    }
    let partial_record = recovery_bytes
        .as_deref()
        .filter(|_| damaged_recovery)
        .and_then(|bytes| serde_json::from_slice::<Value>(bytes).ok());
    if partial_record.as_ref().is_some_and(|value| {
        [
            ("config_path", config_path),
            ("fixed_path", fixed_path.as_path()),
        ]
        .into_iter()
        .any(|(key, expected)| {
            value
                .get(key)
                .and_then(Value::as_str)
                .is_some_and(|path| !paths_eq(Path::new(path), expected))
        })
    }) {
        return Err(CatalogError::OwnershipConflict);
    }
    let recoverable_record = partial_record.filter(|value| {
        value.get("config_path").and_then(Value::as_str).is_some()
            && value.get("fixed_path").and_then(Value::as_str).is_some()
    });
    let recovered_pointer = recoverable_record
        .as_ref()
        .and_then(|value| value.get("original_model_catalog_json"))
        .and_then(Value::as_str)
        .map(Path::new)
        .filter(|path| !paths_eq(path, &fixed_path));
    let original = pointer
        .as_deref()
        .filter(|path| !paths_eq(path, &fixed_path))
        .or_else(|| {
            record
                .as_ref()?
                .original_model_catalog_json
                .as_deref()
                .map(Path::new)
                .filter(|path| !paths_eq(path, &fixed_path))
        })
        .or(recovered_pointer);
    let old_bytes = catalog_storage::read_optional(&fixed_path)?;
    let has_catalog = old_bytes
        .as_deref()
        .is_some_and(|bytes| parse_models(bytes).is_ok_and(|models| !models.is_empty()));
    let recovery_source = if has_catalog {
        old_bytes.clone()
    } else {
        let baseline = record
            .as_ref()
            .map(|record| &record.baseline_document)
            .or_else(|| recoverable_record.as_ref()?.get("baseline_document"))
            .and_then(record_baseline);
        let local = read_repair_source(&fixed_path, original)
            .or_else(|| {
                record
                    .as_ref()?
                    .source_path
                    .as_deref()
                    .and_then(read_usable_catalog)
            })
            .or(baseline)
            .or_else(|| {
                let snapshot = root_snapshot(config_path, bundled_executable)?;
                (!snapshot.models.is_empty())
                    .then(|| serde_json::to_vec_pretty(&snapshot.document).ok())
                    .flatten()
            });
        match local {
            Some(bytes) => Some(bytes),
            None if fallback_models.is_empty() => None,
            None => Some(fallback_catalog(fallback_models)?),
        }
    };
    let replacement =
        prepare_repair_document(recovery_source.ok_or(CatalogError::SourceUnavailable)?)?;
    if read_catalog_pointer(config_path)? != pointer
        || catalog_storage::read_optional(recovery_path)? != recovery_bytes
        || catalog_storage::read_optional(&fixed_path)? != old_bytes
    {
        return Err(CatalogError::ContentChanged);
    }
    let metadata_path = catalog_storage::metadata_path(&fixed_path);
    if let Some(bytes) = catalog_storage::read_optional(&metadata_path)?
        && serde_json::from_slice::<super::CatalogMetadata>(&bytes).is_err()
    {
        backup_damaged(&metadata_path, &bytes)?;
    }
    let metadata = catalog_storage::read_metadata(&fixed_path);
    catalog_storage::write_metadata(&fixed_path, &metadata)?;
    let catalog_changed = old_bytes.as_deref() != Some(replacement.as_slice());
    if catalog_changed {
        if let Some(bytes) = old_bytes.as_deref() {
            backup_damaged(&fixed_path, bytes)?;
        }
        write_atomic(&fixed_path, &replacement)?;
    }
    let rebuild_record = damaged_recovery || (record.is_none() && points_to_turbo);
    if rebuild_record {
        if read_catalog_pointer(config_path)? != pointer
            || catalog_storage::read_optional(recovery_path)? != recovery_bytes
        {
            return Err(CatalogError::ContentChanged);
        }
        let catalog_bytes = fs::read(&fixed_path).map_err(CatalogError::Read)?;
        let mut repaired =
            new_ownership_record(config_path, &fixed_path, original, None, &catalog_bytes)?;
        repaired.released = !points_to_turbo;
        if let Some(bytes) = recovery_bytes.as_deref() {
            backup_damaged(recovery_path, bytes)?;
        }
        write_record(recovery_path, &repaired)?;
    }
    let mut status = if points_to_turbo
        && (rebuild_record || record.as_ref().is_some_and(|record| !record.released))
    {
        // 恢复可用快照后先原样加载；下次同步再合并 Codex 内置目录。
        read_catalog(home, config_path, recovery_path, true, false, false)?
    } else {
        ensure_catalog_with_executable(home, config_path, recovery_path, bundled_executable)?
    };
    status.restart_required = true;
    status.loaded = false;
    Ok(status)
}

fn prepare_repair_document(bytes: Vec<u8>) -> Result<Vec<u8>, CatalogError> {
    let models = parse_models(&bytes)?;
    if models.is_empty() {
        return Err(CatalogError::InvalidSchema("模型目录为空".to_owned()));
    }
    let mut document: Value = serde_json::from_slice(&bytes).map_err(CatalogError::Json)?;
    let entries = document
        .get_mut("models")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| CatalogError::InvalidSchema("缺少 models 数组".to_owned()))?;
    let mut changed = false;
    for (entry, model) in entries.iter_mut().zip(models) {
        let slug = model.slug.clone();
        let mut defaults = model.with_safe_defaults();
        if defaults.display_name.trim().is_empty() {
            defaults.display_name.clone_from(&slug);
        }
        if !matches!(defaults.visibility.as_str(), "list" | "hide" | "none") {
            defaults.visibility = "list".to_owned();
        }
        let mut fields = serde_json::json!({});
        write_model_fields(&mut fields, &defaults);
        for key in missing_codex_fields(entry) {
            entry[key] = if key == "truncation_policy" {
                serde_json::json!({"mode":"tokens", "limit":10000})
            } else {
                fields[key].clone()
            };
            changed = true;
        }
    }
    validate_codex_document(&document)?;
    validate_repair_document(&document)?;
    if changed {
        serde_json::to_vec_pretty(&document).map_err(CatalogError::Json)
    } else {
        Ok(bytes)
    }
}

fn validate_repair_document(document: &Value) -> Result<(), CatalogError> {
    let entries = document
        .get("models")
        .and_then(Value::as_array)
        .ok_or_else(|| CatalogError::InvalidSchema("缺少 models 数组".to_owned()))?;
    for entry in entries {
        if let Some(field) = missing_codex_fields(entry).next() {
            let slug = entry
                .get("slug")
                .and_then(Value::as_str)
                .unwrap_or_default();
            return Err(CatalogError::InvalidSchema(format!(
                "模型 {slug} 缺少有效的 {field}"
            )));
        }
    }
    Ok(())
}

fn missing_codex_fields(entry: &Value) -> impl Iterator<Item = &'static str> + use<> {
    let text = |key| {
        entry
            .get(key)
            .and_then(Value::as_str)
            .is_some_and(|value| !value.trim().is_empty())
    };
    [
        ("display_name", text("display_name")),
        (
            "base_instructions",
            entry.get("base_instructions").is_some_and(Value::is_string),
        ),
        ("shell_type", text("shell_type")),
        (
            "visibility",
            matches!(
                entry.get("visibility").and_then(Value::as_str),
                Some("list" | "hide" | "none")
            ),
        ),
        (
            "priority",
            entry.get("priority").and_then(Value::as_i64).is_some(),
        ),
        (
            "supported_in_api",
            entry.get("supported_in_api").is_some_and(Value::is_boolean),
        ),
        (
            "support_verbosity",
            entry
                .get("support_verbosity")
                .is_some_and(Value::is_boolean),
        ),
        (
            "supported_reasoning_levels",
            entry
                .get("supported_reasoning_levels")
                .and_then(Value::as_array)
                .is_some_and(|levels| {
                    levels
                        .iter()
                        .all(|level| level.get("effort").and_then(Value::as_str).is_some())
                }),
        ),
        (
            "experimental_supported_tools",
            entry
                .get("experimental_supported_tools")
                .and_then(Value::as_array)
                .is_some_and(|tools| tools.iter().all(Value::is_string)),
        ),
        ("truncation_policy", valid_truncation_policy(entry)),
    ]
    .into_iter()
    .filter_map(|(key, valid)| (!valid).then_some(key))
}

fn fallback_catalog(models: &[CatalogModel]) -> Result<Vec<u8>, CatalogError> {
    let entries = models
        .iter()
        .map(|model| {
            let mut model = model.clone().with_safe_defaults();
            if model.display_name.is_empty() {
                model.display_name.clone_from(&model.slug);
            }
            if model.supported_reasoning_levels.is_empty() {
                model.default_reasoning_level = Some("none".to_owned());
            }
            let mut entry = serde_json::json!({});
            write_model_fields(&mut entry, &model);
            if model.context_window.is_none() {
                entry["context_window"] = Value::Null;
            }
            entry
        })
        .collect::<Vec<_>>();
    let document = serde_json::json!({"models": entries});
    serde_json::to_vec_pretty(&document).map_err(CatalogError::Json)
}
