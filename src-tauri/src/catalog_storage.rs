use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

use serde_json::Value;
use tempfile::NamedTempFile;
use toml_edit::{DocumentMut, Item, value};

use super::catalog_diff::{diff_models, models_document};
use super::catalog_types::OwnershipRecord;
use super::{CatalogError, CatalogModel, CatalogStatus};

pub(super) fn status_from_file(
    fixed_path: &Path,
    record: &OwnershipRecord,
    restart_required: bool,
    loaded: bool,
    request_verified: bool,
) -> Result<CatalogStatus, CatalogError> {
    let bytes = fs::read(fixed_path).map_err(CatalogError::Read)?;
    let document: Value = serde_json::from_slice(&bytes).map_err(CatalogError::Json)?;
    let models = parse_models(&bytes)?;
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
        result.push(CatalogModel {
            display_name: string_field(model, "display_name"),
            description: string_field(model, "description"),
            visibility: string_field_or(model, "visibility", "list"),
            priority: model
                .get("priority")
                .and_then(Value::as_i64)
                .unwrap_or_default(),
            slug,
        });
    }
    Ok(result)
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
