use std::{
    fmt, fs,
    io::Write,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;
use toml_edit::{DocumentMut, Item, value};

pub(crate) const FIXED_CATALOG_RELATIVE_PATH: &str = ".codex/model-catalogs/ai_cove_turbo.json";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CatalogModel {
    pub(crate) slug: String,
    pub(crate) display_name: String,
    pub(crate) description: String,
    pub(crate) visibility: String,
    pub(crate) priority: i64,
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
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CatalogModelUpdate {
    pub(crate) slug: String,
    pub(crate) visibility: Option<String>,
    pub(crate) priority: Option<i64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct OwnershipRecord {
    config_path: PathBuf,
    fixed_path: PathBuf,
    original_model_catalog_json: Option<String>,
    source_path: Option<PathBuf>,
    baseline_models: Vec<CatalogModel>,
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
            Self::ContentChanged => write!(formatter, "模型目录已被外部修改，请重新加载后保存"),
        }
    }
}

impl std::error::Error for CatalogError {}

pub(crate) fn fixed_catalog_path(home: &Path) -> PathBuf {
    home.join(FIXED_CATALOG_RELATIVE_PATH)
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
    let mut source_path = None;
    if !fixed_path.exists() {
        let Some(source) = current_pointer.clone() else {
            return Err(CatalogError::SourceUnavailable);
        };
        let bytes = fs::read(&source).map_err(CatalogError::Read)?;
        let _ = parse_models(&bytes)?;
        write_atomic(&fixed_path, &bytes)?;
        source_path = Some(source);
    } else {
        let bytes = fs::read(&fixed_path).map_err(CatalogError::Read)?;
        let _ = parse_models(&bytes)?;
    }

    let bytes = fs::read(&fixed_path).map_err(CatalogError::Read)?;
    let baseline_models = parse_models(&bytes)?;
    let record = OwnershipRecord {
        config_path: config_path.to_path_buf(),
        fixed_path: fixed_path.clone(),
        original_model_catalog_json: current_pointer
            .as_ref()
            .map(|path| path.display().to_string()),
        source_path,
        baseline_models,
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
    write_atomic(
        &fixed_path,
        &serde_json::to_vec_pretty(&document).map_err(CatalogError::Json)?,
    )?;
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
    })
}

fn status_from_file(
    fixed_path: &Path,
    record: &OwnershipRecord,
    restart_required: bool,
    loaded: bool,
    request_verified: bool,
) -> Result<CatalogStatus, CatalogError> {
    let bytes = fs::read(fixed_path).map_err(CatalogError::Read)?;
    let models = parse_models(&bytes)?;
    let changes = diff_models(&record.baseline_models, &models);
    Ok(CatalogStatus {
        path: fixed_path.display().to_string(),
        state: "owned".to_owned(),
        source_path: record
            .source_path
            .as_ref()
            .map(|path| path.display().to_string()),
        models,
        changes,
        restart_required,
        loaded,
        request_verified,
        revision: digest(&bytes),
    })
}

fn parse_models(bytes: &[u8]) -> Result<Vec<CatalogModel>, CatalogError> {
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

fn diff_models(before: &[CatalogModel], after: &[CatalogModel]) -> Vec<CatalogChange> {
    let mut changes = Vec::new();
    for next in after {
        let Some(previous) = before.iter().find(|model| model.slug == next.slug) else {
            changes.push(CatalogChange {
                slug: next.slug.clone(),
                field: "model".to_owned(),
                before: None,
                after: Some(next.display_name.clone()),
            });
            continue;
        };
        for (field, before, after) in [
            (
                "visibility",
                previous.visibility.clone(),
                next.visibility.clone(),
            ),
            (
                "priority",
                previous.priority.to_string(),
                next.priority.to_string(),
            ),
        ] {
            if before != after {
                changes.push(CatalogChange {
                    slug: next.slug.clone(),
                    field: field.to_owned(),
                    before: Some(before),
                    after: Some(after),
                });
            }
        }
    }
    changes
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
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

fn set_string(model: &mut Value, key: &str, value: &str) {
    if let Some(object) = model.as_object_mut() {
        object.insert(key.to_owned(), Value::String(value.to_owned()));
    }
}

fn set_number(model: &mut Value, key: &str, value: i64) {
    if let Some(object) = model.as_object_mut() {
        object.insert(key.to_owned(), json!(value));
    }
}

fn read_catalog_pointer(config_path: &Path) -> Result<Option<PathBuf>, CatalogError> {
    let source = fs::read_to_string(config_path).map_err(CatalogError::Read)?;
    let document = source.parse::<DocumentMut>().map_err(CatalogError::Toml)?;
    Ok(document
        .get("model_catalog_json")
        .and_then(Item::as_str)
        .map(PathBuf::from))
}

fn write_catalog_pointer(config_path: &Path, pointer: Option<&Path>) -> Result<(), CatalogError> {
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

fn read_record(path: &Path) -> Result<Option<OwnershipRecord>, CatalogError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(CatalogError::Read(error)),
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(CatalogError::Json)
}

fn write_record(path: &Path, record: &OwnershipRecord) -> Result<(), CatalogError> {
    write_atomic(
        path,
        &serde_json::to_vec(record).map_err(CatalogError::Json)?,
    )
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), CatalogError> {
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
            r#"{"models":[{"slug":"alpha","display_name":"Alpha","description":"a","visibility":"list","priority":2,"unknown":"kept"},{"slug":"beta","display_name":"Beta","description":"b","visibility":"hide","priority":1}]}"#,
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
}
