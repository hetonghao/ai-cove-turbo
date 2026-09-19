use std::{
    collections::BTreeMap,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use futures_util::StreamExt;
use serde::Serialize;
use sha2::{Digest, Sha256};
use url::Url;

use crate::skills::{SkillError, parse_version, validate_skill_id};
use crate::skills_install::{ensure_safe_ancestors, safe_metadata};

const MAX_CATALOG_BYTES: u64 = 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_SKILL_FILES: usize = 256;
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 32 * 1024 * 1024;
const MAX_PATH_BYTES: usize = 1024;
const MAX_COMPONENT_BYTES: usize = 255;
const MAX_PATH_DEPTH: usize = 32;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone)]
pub(crate) struct CatalogSkill {
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub manifest: String,
    pub manifest_sha256: String,
}

#[derive(Debug, Clone)]
pub(crate) struct RemoteCatalog {
    skills: Vec<CatalogSkill>,
    manifests: BTreeMap<String, RemoteManifest>,
}

impl RemoteCatalog {
    pub(crate) fn skills(&self) -> &[CatalogSkill] {
        &self.skills
    }

    pub(crate) fn skill(&self, id: &str) -> Option<&CatalogSkill> {
        self.skills.iter().find(|skill| skill.id == id)
    }

    pub(crate) fn manifest(&self, manifest_rel: &str) -> Option<&RemoteManifest> {
        self.manifests.get(manifest_rel)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct RemoteManifest {
    pub id: String,
    pub version: String,
    pub files: Vec<RemoteFile>,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RemoteFile {
    pub path: String,
    pub size: u64,
    pub sha256: String,
    pub executable: bool,
}

pub(crate) enum RemoteLoad {
    Fresh(RemoteCatalog),
    Stale(RemoteCatalog, String),
}

impl RemoteLoad {
    pub(crate) fn state(&self) -> &'static str {
        match self {
            RemoteLoad::Fresh(_) => "fresh",
            RemoteLoad::Stale(_, _) => "stale",
        }
    }

    pub(crate) fn is_fresh(&self) -> bool {
        matches!(self, RemoteLoad::Fresh(_))
    }

    pub(crate) fn message(&self) -> Option<&str> {
        match self {
            RemoteLoad::Fresh(_) => None,
            RemoteLoad::Stale(_, message) => Some(message.as_str()),
        }
    }

    pub(crate) fn catalog(&self) -> Option<&RemoteCatalog> {
        match self {
            RemoteLoad::Fresh(catalog) | RemoteLoad::Stale(catalog, _) => Some(catalog),
        }
    }
}

pub(crate) struct CatalogClient {
    base_url: Url,
    client: reqwest::Client,
    cache_dir: PathBuf,
    anchor: PathBuf,
    ttl: Duration,
    memory: tokio::sync::Mutex<Option<MemoryState>>,
}

struct MemoryState {
    catalog: RemoteCatalog,
    fetched: Instant,
    fresh: bool,
}

impl CatalogClient {
    pub(crate) fn new(
        catalog_url: &str,
        cache_dir: PathBuf,
        ttl: Duration,
    ) -> Result<Self, SkillError> {
        let url = Url::parse(catalog_url).map_err(|error| {
            SkillError::new("internal", format!("invalid catalog url: {error}"))
        })?;
        if url.scheme() != "http" && url.scheme() != "https" {
            return Err(SkillError::new(
                "internal",
                "catalog url must use http or https",
            ));
        }
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|error| SkillError::new("internal", format!("http client failed: {error}")))?;
        let leaf = cache_dir
            .file_name()
            .ok_or_else(|| SkillError::new("internal", "skills cache path has no leaf"))?
            .to_os_string();
        let parent = cache_dir
            .parent()
            .ok_or_else(|| SkillError::new("internal", "skills cache path has no parent"))?;
        let anchor = crate::skills_install::canonical_anchor(parent);
        Ok(Self {
            base_url: url,
            client,
            cache_dir: anchor.join(leaf),
            anchor,
            ttl,
            memory: tokio::sync::Mutex::new(None),
        })
    }

    fn resource_url(&self, rel: &str) -> Option<Url> {
        if rel.is_empty() || rel.starts_with('/') || rel.contains('\\') || rel.contains('%') {
            return None;
        }
        for segment in rel.split('/') {
            if segment.is_empty() || segment == "." || segment == ".." {
                return None;
            }
        }
        let prefix = self
            .base_url
            .as_str()
            .trim_end_matches(|c: char| c != '/')
            .to_string();
        let mut url = Url::parse(&format!("{prefix}{rel}")).ok()?;
        if url.scheme() != self.base_url.scheme()
            || url.host_str() != self.base_url.host_str()
            || url.port_or_known_default() != self.base_url.port_or_known_default()
        {
            return None;
        }
        url.set_fragment(None);
        url.set_query(None);
        Some(url)
    }

    async fn get_bytes(&self, url: Url, limit: u64) -> Result<Vec<u8>, SkillError> {
        let response = self.client.get(url).send().await.map_err(|error| {
            SkillError::new(
                "network_error",
                format!("official source unreachable: {error}"),
            )
        })?;
        if !response.status().is_success() {
            return Err(SkillError::new(
                "network_error",
                format!("official source returned {}", response.status()),
            ));
        }
        let mut stream = response.bytes_stream();
        let mut body = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| {
                SkillError::new("network_error", format!("download interrupted: {error}"))
            })?;
            if body.len() as u64 + chunk.len() as u64 > limit {
                return Err(SkillError::new(
                    "integrity_error",
                    "official resource exceeds the allowed size",
                ));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    async fn fetch_full(&self) -> Result<RemoteCatalog, SkillError> {
        let raw = self
            .get_bytes(self.base_url.clone(), MAX_CATALOG_BYTES)
            .await?;
        let skills = parse_catalog(&raw)?;
        let mut manifests = BTreeMap::new();
        let mut manifest_blobs: Vec<(String, Vec<u8>)> = Vec::new();
        for entry in &skills {
            let url = self.resource_url(&entry.manifest).ok_or_else(|| {
                SkillError::new("unsafe_path", "manifest path escapes the official source")
            })?;
            let body = self.get_bytes(url, MAX_MANIFEST_BYTES).await?;
            let manifest = parse_manifest(&body, entry)?;
            manifests.insert(entry.manifest.clone(), manifest);
            manifest_blobs.push((entry.manifest_sha256.clone(), body));
        }
        self.persist(&raw, &manifest_blobs);
        Ok(RemoteCatalog { skills, manifests })
    }

    fn persist(&self, catalog_raw: &[u8], manifests: &[(String, Vec<u8>)]) {
        let manifests_dir = self.cache_dir.join("manifests");
        if ensure_safe_ancestors(&self.anchor, &manifests_dir).is_err() {
            return;
        }
        if std::fs::create_dir_all(&manifests_dir).is_err() {
            return;
        }
        for (sha, body) in manifests {
            let target = manifests_dir.join(format!("{sha}.json"));
            if ensure_safe_ancestors(&self.anchor, &target).is_err() {
                return;
            }
            match safe_metadata(&self.anchor, &target) {
                Err(_) => return,
                Ok(Some(metadata)) if !metadata.is_file() => return,
                _ => {}
            }
            let Ok(mut tmp) = tempfile::NamedTempFile::new_in(&manifests_dir) else {
                return;
            };
            if tmp
                .write_all(body)
                .and_then(|_| tmp.as_file().sync_all())
                .is_err()
            {
                return;
            }
            if tmp.persist(&target).is_err() {
                return;
            }
        }
        let target = self.cache_dir.join("catalog.json");
        if ensure_safe_ancestors(&self.anchor, &target).is_err() {
            return;
        }
        match safe_metadata(&self.anchor, &target) {
            Err(_) => return,
            Ok(Some(metadata)) if !metadata.is_file() => return,
            _ => {}
        }
        let Ok(mut tmp) = tempfile::NamedTempFile::new_in(&self.cache_dir) else {
            return;
        };
        if tmp
            .write_all(catalog_raw)
            .and_then(|_| tmp.as_file().sync_all())
            .is_err()
        {
            return;
        }
        let _ = tmp.persist(&target);
    }

    fn bounded_read(&self, path: &Path, limit: u64) -> Option<Vec<u8>> {
        if ensure_safe_ancestors(&self.anchor, path).is_err() {
            return None;
        }
        match safe_metadata(&self.anchor, path) {
            Err(_) | Ok(None) => return None,
            Ok(Some(metadata)) if !metadata.is_file() => return None,
            _ => {}
        }
        let file = std::fs::File::open(path).ok()?;
        let mut data = Vec::new();
        file.take(limit + 1).read_to_end(&mut data).ok()?;
        if data.len() as u64 > limit {
            return None;
        }
        Some(data)
    }

    fn load_cached(&self) -> Option<RemoteCatalog> {
        let raw = self.bounded_read(&self.cache_dir.join("catalog.json"), MAX_CATALOG_BYTES)?;
        let skills = parse_catalog(&raw).ok()?;
        let mut manifests = BTreeMap::new();
        for entry in &skills {
            let name = format!("{}.json", entry.manifest_sha256);
            let body = self.bounded_read(
                &self.cache_dir.join("manifests").join(&name),
                MAX_MANIFEST_BYTES,
            )?;
            let manifest = parse_manifest(&body, entry).ok()?;
            manifests.insert(entry.manifest.clone(), manifest);
        }
        Some(RemoteCatalog { skills, manifests })
    }

    pub(crate) async fn load(&self, refresh: bool) -> Result<RemoteLoad, SkillError> {
        if !refresh {
            let memory = self.memory.lock().await;
            if let Some(state) = memory.as_ref() {
                if state.fresh && state.fetched.elapsed() < self.ttl {
                    return Ok(RemoteLoad::Fresh(state.catalog.clone()));
                }
            }
        }
        match self.fetch_full().await {
            Ok(catalog) => {
                *self.memory.lock().await = Some(MemoryState {
                    catalog: catalog.clone(),
                    fetched: Instant::now(),
                    fresh: true,
                });
                Ok(RemoteLoad::Fresh(catalog))
            }
            Err(error) => {
                let memory_catalog = {
                    let mut memory = self.memory.lock().await;
                    if let Some(state) = memory.as_mut() {
                        state.fresh = false;
                        Some(state.catalog.clone())
                    } else {
                        None
                    }
                };
                let message = format!(
                    "未能确认最新版本：{}；展示上次成功获取的官方目录",
                    error.message
                );
                match memory_catalog.or_else(|| self.load_cached()) {
                    Some(cached) => Ok(RemoteLoad::Stale(cached, message)),
                    None => Err(error),
                }
            }
        }
    }

    pub(crate) async fn download_files(
        &self,
        anchor: &Path,
        manifest: &RemoteManifest,
        staging_dir: &Path,
    ) -> Result<(), CatalogStageError> {
        if manifest.files.is_empty() || manifest.files.len() > MAX_SKILL_FILES {
            return Err(CatalogStageError::integrity(
                "manifest file count is out of bounds",
            ));
        }
        let mut total: u64 = 0;
        for file in &manifest.files {
            if file.size == 0 || file.size > MAX_FILE_BYTES {
                return Err(CatalogStageError::integrity(format!(
                    "skill file {} has an invalid size",
                    file.path
                )));
            }
            total = total.saturating_add(file.size);
            if total > MAX_TOTAL_BYTES {
                return Err(CatalogStageError::integrity(
                    "skill download exceeds the total size limit",
                ));
            }
        }
        match safe_metadata(anchor, staging_dir) {
            Ok(Some(metadata)) if metadata.is_dir() => {}
            Ok(_) => {
                return Err(CatalogStageError::unsafe_path(
                    "staging directory is not a prepared directory",
                ));
            }
            Err(error) => return Err(CatalogStageError::unsafe_path(error.message)),
        }
        let rel_prefix = format!("releases/{}/{}/files/", manifest.id, manifest.version);
        for file in &manifest.files {
            validate_relative_path(&file.path)
                .map_err(|message| CatalogStageError::unsafe_path(message))?;
            let url = self
                .resource_url(&format!("{rel_prefix}{}", file.path))
                .ok_or_else(|| {
                    CatalogStageError::unsafe_path(format!("unsafe file path: {}", file.path))
                })?;
            let data = self
                .get_bytes(url, MAX_FILE_BYTES + 1)
                .await
                .map_err(|error| CatalogStageError::network(error.message))?;
            if data.len() as u64 != file.size {
                return Err(CatalogStageError::integrity(format!(
                    "downloaded size mismatch for {}",
                    file.path
                )));
            }
            if sha256_hex(&data) != file.sha256 {
                return Err(CatalogStageError::integrity(format!(
                    "downloaded checksum mismatch for {}",
                    file.path
                )));
            }
            let target = file
                .path
                .split('/')
                .fold(staging_dir.to_path_buf(), |mut path, part| {
                    path.push(part);
                    path
                });
            if let Some(parent) = target.parent() {
                ensure_safe_ancestors(anchor, parent)
                    .map_err(|error| CatalogStageError::unsafe_path(error.message))?;
                std::fs::create_dir_all(parent).map_err(|error| {
                    CatalogStageError::permission(format!(
                        "cannot create skill directories: {error}"
                    ))
                })?;
                ensure_safe_ancestors(anchor, parent)
                    .map_err(|error| CatalogStageError::unsafe_path(error.message))?;
            }
            match safe_metadata(anchor, &target) {
                Ok(None) => {}
                Ok(_) => {
                    return Err(CatalogStageError::unsafe_path(format!(
                        "staged path already exists: {}",
                        file.path
                    )));
                }
                Err(error) => return Err(CatalogStageError::unsafe_path(error.message)),
            }
            let mut handle = std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&target)
                .map_err(|error| {
                    CatalogStageError::permission(format!(
                        "cannot write staged file {}: {error}",
                        file.path
                    ))
                })?;
            handle
                .write_all(&data)
                .and_then(|_| handle.sync_all())
                .map_err(|error| {
                    CatalogStageError::permission(format!(
                        "cannot write staged file {}: {error}",
                        file.path
                    ))
                })?;
            drop(handle);
            #[cfg(unix)]
            if file.executable {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).map_err(
                    |error| {
                        CatalogStageError::permission(format!(
                            "cannot mark staged file executable {}: {error}",
                            file.path
                        ))
                    },
                )?;
            }
        }
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct CatalogStageError {
    code: &'static str,
    message: String,
}

impl CatalogStageError {
    fn integrity(message: impl Into<String>) -> Self {
        Self {
            code: "integrity_error",
            message: message.into(),
        }
    }
    fn unsafe_path(message: impl Into<String>) -> Self {
        Self {
            code: "unsafe_path",
            message: message.into(),
        }
    }
    fn network(message: impl Into<String>) -> Self {
        Self {
            code: "network_error",
            message: message.into(),
        }
    }
    fn permission(message: impl Into<String>) -> Self {
        Self {
            code: "permission_denied",
            message: message.into(),
        }
    }

    pub(crate) fn code(&self) -> &'static str {
        self.code
    }
    pub(crate) fn message(self) -> String {
        self.message
    }
}

pub(crate) fn sha256_hex(data: &[u8]) -> String {
    let mut out = String::with_capacity(64);
    for byte in Sha256::digest(data) {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn parse_catalog(raw: &[u8]) -> Result<Vec<CatalogSkill>, SkillError> {
    let value: serde_json::Value = serde_json::from_slice(raw)
        .map_err(|_| SkillError::new("integrity_error", "catalog is not valid JSON"))?;
    if value.get("schema_version").and_then(|v| v.as_u64()) != Some(1) {
        return Err(SkillError::new(
            "integrity_error",
            "catalog schema_version is not supported",
        ));
    }
    let skills = value
        .get("skills")
        .and_then(|skills| skills.as_array())
        .ok_or_else(|| SkillError::new("integrity_error", "catalog is missing skills"))?;
    let mut parsed = Vec::new();
    let mut seen_ids = std::collections::BTreeSet::new();
    for skill in skills {
        let entry = CatalogSkill {
            id: required_string(skill, "id")?,
            name: required_string(skill, "name")?,
            description: required_string(skill, "description")?,
            version: required_string(skill, "version")?,
            manifest: required_string(skill, "manifest")?,
            manifest_sha256: required_string(skill, "manifest_sha256")?,
        };
        validate_skill_id(&entry.id).map_err(|e| SkillError::new("integrity_error", e.message))?;
        parse_version(&entry.version)?;
        validate_manifest_path(&entry.manifest, &entry.id, &entry.version)?;
        validate_sha256(&entry.manifest_sha256)?;
        if !seen_ids.insert(entry.id.clone()) {
            return Err(SkillError::new(
                "integrity_error",
                format!("duplicate skill id in catalog: {}", entry.id),
            ));
        }
        parsed.push(entry);
    }
    if parsed.len() > 64 {
        return Err(SkillError::new(
            "integrity_error",
            "catalog declares too many skills",
        ));
    }
    Ok(parsed)
}

fn parse_manifest(raw: &[u8], entry: &CatalogSkill) -> Result<RemoteManifest, SkillError> {
    let sha = sha256_hex(raw);
    if sha != entry.manifest_sha256 {
        return Err(SkillError::new(
            "integrity_error",
            "manifest checksum does not match the catalog",
        ));
    }
    let value: serde_json::Value = serde_json::from_slice(raw)
        .map_err(|_| SkillError::new("integrity_error", "manifest is not valid JSON"))?;
    if value.get("schema_version").and_then(|v| v.as_u64()) != Some(1) {
        return Err(SkillError::new(
            "integrity_error",
            "manifest schema_version is not supported",
        ));
    }
    let id = required_string(&value, "id")?;
    let version = required_string(&value, "version")?;
    if id != entry.id || version != entry.version {
        return Err(SkillError::new(
            "integrity_error",
            "manifest id/version does not match the catalog entry",
        ));
    }
    let files = value
        .get("files")
        .and_then(|files| files.as_array())
        .ok_or_else(|| SkillError::new("integrity_error", "manifest is missing files"))?;
    let mut parsed = Vec::with_capacity(files.len());
    let mut total: u64 = 0;
    for file in files {
        let path = required_string(file, "path")?;
        let size = file
            .get("size")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| SkillError::new("integrity_error", "manifest file is missing size"))?;
        if size == 0 || size > MAX_FILE_BYTES {
            return Err(SkillError::new(
                "integrity_error",
                format!("manifest file {path} has an invalid size"),
            ));
        }
        total = total.saturating_add(size);
        if total > MAX_TOTAL_BYTES {
            return Err(SkillError::new(
                "integrity_error",
                "manifest exceeds the total size limit",
            ));
        }
        let sha256 = required_string(file, "sha256")?;
        validate_sha256(&sha256)?;
        let executable = file
            .get("executable")
            .and_then(serde_json::Value::as_bool)
            .ok_or_else(|| {
                SkillError::new("integrity_error", "manifest executable must be a boolean")
            })?;
        parsed.push(RemoteFile {
            path,
            size,
            sha256,
            executable,
        });
    }
    validate_file_paths(parsed.iter().map(|file| file.path.as_str()))?;
    if !parsed.iter().any(|file| file.path == "SKILL.md") {
        return Err(SkillError::new(
            "integrity_error",
            "manifest does not declare a SKILL.md",
        ));
    }
    parsed.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(RemoteManifest {
        id,
        version,
        files: parsed,
        sha256: sha,
    })
}

pub(crate) fn validate_file_paths<'a>(
    paths: impl Iterator<Item = &'a str>,
) -> Result<(), SkillError> {
    let mut folded_files = std::collections::BTreeSet::new();
    let mut folded_dirs = std::collections::BTreeMap::new();
    let mut count = 0usize;
    for path in paths {
        count += 1;
        if count > MAX_SKILL_FILES {
            return Err(SkillError::new(
                "integrity_error",
                "manifest file count is out of bounds",
            ));
        }
        validate_relative_path(path).map_err(|message| SkillError::new("unsafe_path", message))?;
        let folded = path.to_lowercase();
        if !folded_files.insert(folded.clone()) {
            return Err(SkillError::new(
                "integrity_error",
                format!("duplicate or case-conflicting path: {path}"),
            ));
        }
        if folded_dirs.contains_key(&folded) {
            return Err(SkillError::new(
                "integrity_error",
                format!("path collides with a directory: {path}"),
            ));
        }
        let mut prefix = String::new();
        let parts: Vec<&str> = path.split('/').collect();
        for part in &parts[..parts.len() - 1] {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(part);
            let folded_dir = prefix.to_lowercase();
            if folded_files.contains(&folded_dir) {
                return Err(SkillError::new(
                    "integrity_error",
                    format!("directory collides with a file path: {path}"),
                ));
            }
            match folded_dirs.get(&folded_dir) {
                Some(existing) if *existing != prefix => {
                    return Err(SkillError::new(
                        "integrity_error",
                        format!("case-conflicting directories under: {path}"),
                    ));
                }
                Some(_) => {}
                None => {
                    folded_dirs.insert(folded_dir, prefix.clone());
                }
            }
        }
    }
    Ok(())
}

fn required_string(value: &serde_json::Value, key: &str) -> Result<String, SkillError> {
    let text = value
        .get(key)
        .and_then(|value| value.as_str())
        .ok_or_else(|| SkillError::new("integrity_error", format!("entry missing {key}")))?;
    if text.trim().is_empty() {
        return Err(SkillError::new(
            "integrity_error",
            format!("entry {key} must not be blank"),
        ));
    }
    Ok(text.to_string())
}

fn validate_manifest_path(rel: &str, id: &str, version: &str) -> Result<(), SkillError> {
    let expected = format!("releases/{id}/{version}/manifest.json");
    if rel == expected {
        Ok(())
    } else {
        Err(SkillError::new(
            "integrity_error",
            format!("manifest path {rel} does not match the declared skill"),
        ))
    }
}

pub(crate) fn validate_sha256(value: &str) -> Result<(), SkillError> {
    let valid = value.len() == 64
        && value
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c));
    if valid {
        Ok(())
    } else {
        Err(SkillError::new(
            "integrity_error",
            "invalid manifest sha256",
        ))
    }
}

fn valid_path_component(part: &str) -> bool {
    if part.len() > MAX_COMPONENT_BYTES {
        return false;
    }
    let mut chars = part.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    let valid_first = first.is_ascii_alphanumeric() || first == '_' || first == '-';
    valid_first && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
}

pub(crate) fn validate_relative_path(path: &str) -> Result<(), String> {
    if path.is_empty() || path.len() > MAX_PATH_BYTES || path.starts_with('/') {
        return Err(format!("unsafe skill file path: {path}"));
    }
    let mut depth = 0usize;
    for part in path.split('/') {
        depth += 1;
        if depth > MAX_PATH_DEPTH {
            return Err(format!("skill file path is too deep: {path}"));
        }
        if part.is_empty() || part == "." || part == ".." {
            return Err(format!("unsafe skill file path: {path}"));
        }
        if !valid_path_component(part) {
            return Err(format!("unsafe skill file path: {path}"));
        }
        if part.starts_with('.') {
            return Err(format!("hidden skill file path is not allowed: {path}"));
        }
        if part.ends_with('.') {
            return Err(format!("unsafe skill file path: {path}"));
        }
        let stem = part
            .split('.')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if matches!(
            stem.as_str(),
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
            return Err(format!("reserved skill file path: {path}"));
        }
        if part == "__pycache__"
            || part.eq_ignore_ascii_case("tests")
            || part.eq_ignore_ascii_case("test")
        {
            return Err(format!("non-publishable skill file path: {path}"));
        }
    }
    let lowered = path.to_ascii_lowercase();
    if lowered.ends_with(".pyc") || lowered.ends_with(".pyo") {
        return Err(format!("non-publishable skill file path: {path}"));
    }
    Ok(())
}
