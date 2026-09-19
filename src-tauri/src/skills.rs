use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::skills_catalog::{CatalogClient, RemoteCatalog};
use crate::skills_install::{self, InstallRoots, SkillLocalState};

pub(crate) const SKILLS_CATALOG_URL: &str = "https://api.ai-cove.com/sidecars/skills/catalog.json";
const SKILLS_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(300);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SkillError {
    pub code: String,
    pub message: String,
}

impl SkillError {
    pub(crate) fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for SkillError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for SkillError {}

pub(crate) fn validate_skill_id(id: &str) -> Result<&str, SkillError> {
    let valid = !id.is_empty()
        && id.len() <= 64
        && !id.starts_with('-')
        && !id.ends_with('-')
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !valid {
        return Err(SkillError::new(
            "invalid_input",
            format!("invalid skill id: {id}"),
        ));
    }
    if matches!(
        id,
        "con"
            | "prn"
            | "aux"
            | "nul"
            | "com1"
            | "com2"
            | "com3"
            | "com4"
            | "com5"
            | "com6"
            | "com7"
            | "com8"
            | "com9"
            | "lpt1"
            | "lpt2"
            | "lpt3"
            | "lpt4"
            | "lpt5"
            | "lpt6"
            | "lpt7"
            | "lpt8"
            | "lpt9"
    ) {
        return Err(SkillError::new(
            "invalid_input",
            format!("reserved skill id: {id}"),
        ));
    }
    Ok(id)
}

pub(crate) fn parse_version(version: &str) -> Result<[u64; 3], SkillError> {
    let parts: Vec<&str> = version.split('.').collect();
    if parts.len() != 3 {
        return Err(SkillError::new(
            "integrity_error",
            format!("invalid skill version: {version}"),
        ));
    }
    let mut out = [0u64; 3];
    for (index, part) in parts.iter().enumerate() {
        if part.is_empty()
            || part.len() > 20
            || (part.len() > 1 && part.starts_with('0'))
            || !part.chars().all(|c| c.is_ascii_digit())
        {
            return Err(SkillError::new(
                "integrity_error",
                format!("invalid skill version: {version}"),
            ));
        }
        out[index] = part.parse().map_err(|_| {
            SkillError::new(
                "integrity_error",
                format!("invalid skill version: {version}"),
            )
        })?;
    }
    Ok(out)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SkillFileChange {
    pub path: String,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SkillStatus {
    pub id: String,
    pub name: String,
    pub description: String,
    pub installed_version: Option<String>,
    pub latest_version: Option<String>,
    pub local_state: String,
    pub update_state: String,
    pub local_revision: String,
    pub release_revision: Option<String>,
    pub local_changes: Vec<SkillFileChange>,
    pub latest_diff: Vec<SkillFileChange>,
    pub backup_path: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SkillsStatus {
    pub install_root: String,
    pub catalog_state: String,
    pub catalog_message: Option<String>,
    pub skills: Vec<SkillStatus>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SkillMutationResult {
    pub status: SkillsStatus,
    pub backup_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InstallReceipt {
    pub schema_version: u32,
    pub source: String,
    pub id: String,
    pub version: String,
    pub manifest_sha256: String,
    pub files: Vec<ReceiptFile>,
    pub installed_at_unix_seconds: u64,
    pub transaction_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReceiptFile {
    pub path: String,
    pub size: u64,
    pub sha256: String,
    pub executable: bool,
}

pub(crate) struct SkillsManager {
    catalog: CatalogClient,
    roots: InstallRoots,
    mutation_lock: Mutex<()>,
}

impl SkillsManager {
    pub(crate) fn new(home: PathBuf, data_dir: PathBuf) -> Result<Self, SkillError> {
        let roots = InstallRoots::new(
            home.join(".agents").join("skills"),
            home.join(".agents").join(".ai-cove-skills"),
            home,
        )?;
        Self::with_source(SKILLS_CATALOG_URL, roots, data_dir.join("skills"))
    }

    #[cfg(test)]
    pub(crate) fn roots_mut(&mut self) -> &mut InstallRoots {
        &mut self.roots
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        catalog_url: &str,
        install_root: PathBuf,
        control_root: PathBuf,
        cache_dir: PathBuf,
    ) -> Result<Self, SkillError> {
        let anchor = install_root
            .parent()
            .and_then(|agents| agents.parent())
            .unwrap_or(&install_root)
            .to_path_buf();
        Self::with_source(
            catalog_url,
            InstallRoots::new(install_root, control_root, anchor)?,
            cache_dir,
        )
    }

    fn with_source(
        catalog_url: &str,
        roots: InstallRoots,
        cache_dir: PathBuf,
    ) -> Result<Self, SkillError> {
        let catalog = CatalogClient::new(catalog_url, cache_dir, SKILLS_CACHE_TTL)?;
        Ok(Self {
            catalog,
            roots,
            mutation_lock: Mutex::new(()),
        })
    }

    async fn snapshot(&self, refresh: bool) -> Result<SkillsStatus, SkillError> {
        let (catalog_state, catalog_message, catalog, fresh) =
            match self.catalog.load(refresh).await {
                Ok(remote) => (
                    remote.state().to_string(),
                    remote.message().map(str::to_string),
                    remote.catalog().cloned(),
                    remote.is_fresh(),
                ),
                Err(error) => ("unavailable".to_string(), Some(error.message), None, false),
            };
        Ok(SkillsStatus {
            install_root: self.roots.install_root.display().to_string(),
            catalog_state,
            catalog_message,
            skills: self.collect_skill_status(catalog.as_ref(), fresh),
        })
    }

    fn collect_skill_status(
        &self,
        catalog: Option<&RemoteCatalog>,
        fresh: bool,
    ) -> Vec<SkillStatus> {
        let mut ids: Vec<String> = Vec::new();
        if let Some(catalog) = catalog {
            for entry in catalog.skills() {
                ids.push(entry.id.clone());
            }
        }
        if let Ok(locals) = skills_install::scan_installed_ids(&self.roots) {
            for id in locals {
                if !ids.iter().any(|known| known == &id) {
                    ids.push(id);
                }
            }
        }
        ids.sort();
        ids.into_iter()
            .map(|id| self.skill_status(&id, catalog, fresh))
            .collect()
    }

    fn skill_status(&self, id: &str, catalog: Option<&RemoteCatalog>, fresh: bool) -> SkillStatus {
        let remote_entry = catalog.and_then(|catalog| catalog.skill(id));
        let remote_manifest = remote_entry
            .and_then(|entry| catalog.and_then(|catalog| catalog.manifest(&entry.manifest)));
        let local = skills_install::inspect(&self.roots, id);
        let local_changes = local
            .changes_vs_receipt()
            .into_iter()
            .map(|(path, kind)| SkillFileChange {
                path,
                kind: kind.to_string(),
            })
            .collect();
        let latest_diff = remote_manifest
            .map(|manifest| {
                local
                    .changes_vs_remote(manifest)
                    .into_iter()
                    .map(|(path, kind)| SkillFileChange {
                        path,
                        kind: kind.to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        SkillStatus {
            id: id.to_string(),
            name: remote_entry
                .map(|entry| entry.name.clone())
                .unwrap_or_else(|| id.to_string()),
            description: remote_entry
                .map(|entry| entry.description.clone())
                .unwrap_or_default(),
            installed_version: local.installed_version(),
            latest_version: remote_entry.map(|entry| entry.version.clone()),
            local_state: local.state_name(),
            update_state: update_state(&local, remote_entry, remote_manifest, fresh),
            local_revision: local.local_revision(),
            release_revision: remote_manifest.map(|manifest| manifest.sha256.clone()),
            local_changes,
            latest_diff,
            backup_path: local.backup_path(),
            error: local.error_message(),
        }
    }

    pub(crate) async fn status(&self, refresh: bool) -> Result<SkillsStatus, SkillError> {
        self.snapshot(refresh).await
    }

    pub(crate) async fn install(
        &self,
        id: &str,
        expected_release_revision: &str,
        expected_local_revision: &str,
        confirm_replace: bool,
    ) -> Result<SkillMutationResult, SkillError> {
        validate_skill_id(id)?;
        let _permit = self.mutation_lock.lock().await;
        let guard = skills_install::begin_mutation(&self.roots)?;
        let remote = self.catalog.load(false).await?;
        let catalog = remote.catalog().cloned().ok_or_else(|| {
            SkillError::new(
                "catalog_unavailable",
                "the official skills catalog could not be verified; reconnect and retry",
            )
        })?;
        if !remote.is_fresh() {
            return Err(SkillError::new(
                "catalog_unavailable",
                "官方技能目录不是最新确认状态，请联网后重试安装",
            ));
        }
        let entry = catalog.skill(id).ok_or_else(|| {
            SkillError::new(
                "not_found",
                format!("skill {id} is not published by the official catalog"),
            )
        })?;
        let manifest = catalog.manifest(&entry.manifest).ok_or_else(|| {
            SkillError::new(
                "integrity_error",
                format!("manifest for {id} could not be loaded"),
            )
        })?;
        if manifest.sha256 != expected_release_revision {
            return Err(SkillError::new(
                "revision_changed",
                "the published revision changed since the last check; review the differences again",
            ));
        }
        let tx = skills_install::new_tx_id();
        let staging = skills_install::prepare_staging(&self.roots, &tx)?;
        if let Err(error) = self
            .catalog
            .download_files(self.roots.anchor(), manifest, &staging)
            .await
            .map_err(|error| SkillError::new(error.code(), error.message()))
        {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(error);
        }
        let backup_path = match skills_install::install_staged(
            &self.roots,
            &guard,
            &tx,
            manifest,
            expected_local_revision,
            confirm_replace,
        )
        .await
        {
            Ok(backup) => backup,
            Err(error) => {
                if crate::skills_install::safe_metadata(self.roots.anchor(), &staging)
                    .map(|meta| meta.is_some())
                    .unwrap_or(false)
                {
                    let _ = std::fs::remove_dir_all(&staging);
                }
                return Err(error);
            }
        };
        let status = self.snapshot(false).await?;
        drop(guard);
        Ok(SkillMutationResult {
            status,
            backup_path: backup_path.map(|path| path.display().to_string()),
        })
    }

    pub(crate) async fn uninstall(
        &self,
        id: &str,
        expected_local_revision: &str,
        confirmed: bool,
    ) -> Result<SkillMutationResult, SkillError> {
        validate_skill_id(id)?;
        let _permit = self.mutation_lock.lock().await;
        if !confirmed {
            return Err(SkillError::new(
                "local_conflict",
                "uninstalling a skill requires an explicit confirmation",
            ));
        }
        let remote = self.catalog.load(false).await.ok();
        let in_catalog = remote
            .as_ref()
            .and_then(|load| load.catalog())
            .map(|catalog| catalog.skill(id).is_some())
            .unwrap_or(false);
        let local = skills_install::inspect(&self.roots, id);
        let receipt_match = local
            .receipt_id()
            .map(|receipt_id| receipt_id == id)
            .unwrap_or(false);
        if !in_catalog && !receipt_match {
            return Err(SkillError::new(
                "not_found",
                format!("skill {id} is not managed by the official skills install"),
            ));
        }
        let guard = skills_install::begin_mutation(&self.roots)?;
        let backup_path =
            skills_install::uninstall(&self.roots, &guard, id, expected_local_revision).await?;
        let status = self.snapshot(false).await?;
        drop(guard);
        Ok(SkillMutationResult {
            status,
            backup_path: Some(backup_path.display().to_string()),
        })
    }
}

fn update_state(
    local: &skills_install::LocalSkill,
    remote_entry: Option<&crate::skills_catalog::CatalogSkill>,
    remote_manifest: Option<&crate::skills_catalog::RemoteManifest>,
    fresh: bool,
) -> String {
    if !fresh {
        return "unknown".to_string();
    }
    if remote_entry.is_none() || remote_manifest.is_none() {
        if local.state == SkillLocalState::Absent {
            return "unavailable".to_string();
        }
        return "unknown".to_string();
    }
    let remote = remote_manifest.unwrap();
    match local.installed_version() {
        None => "available".to_string(),
        Some(version) => match (parse_version(&version), parse_version(&remote.version)) {
            (Ok(local_v), Ok(remote_v)) => match local_v.cmp(&remote_v) {
                std::cmp::Ordering::Less => "available".to_string(),
                std::cmp::Ordering::Equal => "current".to_string(),
                std::cmp::Ordering::Greater => "local_newer".to_string(),
            },
            _ => "unknown".to_string(),
        },
    }
}

pub(crate) fn unix_seconds_now() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

pub(crate) fn receipt_source() -> &'static str {
    "https://api.ai-cove.com/sidecars/skills/"
}
