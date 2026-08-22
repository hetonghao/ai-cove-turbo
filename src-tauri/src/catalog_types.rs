use std::{fmt, path::PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

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
pub(crate) struct OwnershipRecord {
    pub(crate) config_path: PathBuf,
    pub(crate) fixed_path: PathBuf,
    pub(crate) original_model_catalog_json: Option<String>,
    pub(crate) source_path: Option<PathBuf>,
    pub(crate) baseline_models: Vec<CatalogModel>,
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
