use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::skills::{
    InstallReceipt, ReceiptFile, SkillError, receipt_source, unix_seconds_now, validate_skill_id,
};
use crate::skills_catalog::{RemoteManifest, sha256_hex};

const RECEIPT_NAME: &str = ".ai-cove-install.json";
const LOCK_NAME: &str = "install.lock";
const MAX_LOCAL_FILE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Default)]
pub(crate) struct Failpoints {
    pub fail_journal_write: bool,
    pub fail_after_old_rename: bool,
    pub fail_activation: bool,
    pub fail_post_verify: bool,
    pub fail_backup_move: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct InstallRoots {
    pub install_root: PathBuf,
    pub control_root: PathBuf,
    anchor: PathBuf,
    pub(crate) failpoints: Failpoints,
}

pub(crate) fn canonical_anchor(path: &Path) -> PathBuf {
    let mut probe = path.to_path_buf();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    while !probe.exists() {
        match probe.file_name() {
            Some(name) => {
                tail.push(name.to_os_string());
                probe = probe.parent().map(Path::to_path_buf).unwrap_or(probe);
            }
            None => break,
        }
    }
    let mut base = std::fs::canonicalize(&probe).unwrap_or(probe);
    for part in tail.iter().rev() {
        base.push(part);
    }
    base
}

impl InstallRoots {
    pub(crate) fn new(
        install_root: PathBuf,
        control_root: PathBuf,
        anchor: PathBuf,
    ) -> Result<Self, SkillError> {
        let canonical = canonical_anchor(&anchor);
        let rebase = |path: &PathBuf| -> Result<PathBuf, SkillError> {
            let rel = path.strip_prefix(&anchor).map_err(|_| {
                SkillError::new(
                    "internal",
                    "skills paths must live under the user home anchor",
                )
            })?;
            Ok(rel.iter().fold(canonical.clone(), |mut path, part| {
                path.push(part);
                path
            }))
        };
        Ok(Self {
            install_root: rebase(&install_root)?,
            control_root: rebase(&control_root)?,
            anchor: canonical,
            failpoints: Failpoints::default(),
        })
    }

    pub(crate) fn quarantine_dir(&self, tx: &str) -> PathBuf {
        self.control_root.join("quarantine").join(tx)
    }

    pub(crate) fn anchor(&self) -> &Path {
        &self.anchor
    }

    pub(crate) fn staging_dir(&self, tx: &str) -> PathBuf {
        self.control_root.join("staging").join(tx)
    }
    fn backup_dir(&self, id: &str, tx: &str) -> PathBuf {
        self.control_root.join("backups").join(id).join(tx)
    }
    fn transactions_dir(&self) -> PathBuf {
        self.control_root.join("transactions")
    }
    fn lock_path(&self) -> PathBuf {
        self.control_root.join(LOCK_NAME)
    }
    fn journal_path(&self, tx: &str) -> PathBuf {
        self.transactions_dir().join(format!("{tx}.json"))
    }
}

pub(crate) fn prepare_staging(roots: &InstallRoots, tx: &str) -> Result<PathBuf, SkillError> {
    if !valid_tx(tx) {
        return Err(SkillError::new("internal", "invalid transaction id"));
    }
    let parent = roots.control_root.join("staging");
    ensure_safe_ancestors(roots.anchor(), &parent)?;
    std::fs::create_dir_all(&parent).map_err(|error| {
        SkillError::new(
            "permission_denied",
            format!("cannot create staging root: {error}"),
        )
    })?;
    ensure_safe_ancestors(roots.anchor(), &parent)?;
    let staging = roots.staging_dir(tx);
    match safe_metadata(roots.anchor(), &staging)? {
        Some(_) => {
            return Err(SkillError::new(
                "unsafe_path",
                "staging path already exists",
            ));
        }
        None => {}
    }
    std::fs::create_dir(&staging).map_err(|error| {
        SkillError::new(
            "permission_denied",
            format!("cannot create staging directory: {error}"),
        )
    })?;
    Ok(staging)
}

fn is_link(metadata: &std::fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    false
}

pub(crate) fn ensure_safe_ancestors(anchor: &Path, path: &Path) -> Result<(), SkillError> {
    match std::fs::symlink_metadata(anchor) {
        Ok(metadata) if is_link(&metadata) || !metadata.is_dir() => {
            return Err(SkillError::new(
                "unsafe_path",
                format!(
                    "trusted skills root is not a directory: {}",
                    anchor.display()
                ),
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(SkillError::new(
                "permission_denied",
                format!("cannot inspect trusted skills root: {error}"),
            ));
        }
    }
    let rel = path.strip_prefix(anchor).map_err(|_| {
        SkillError::new(
            "unsafe_path",
            format!("path escapes the trusted root: {}", path.display()),
        )
    })?;
    let mut current = anchor.to_path_buf();
    let components: Vec<_> = rel.components().collect();
    for (index, component) in components.iter().enumerate() {
        let name = component.as_os_str().to_str().ok_or_else(|| {
            SkillError::new(
                "unsafe_path",
                "non-UTF-8 path component under the skills root",
            )
        })?;
        if name.is_empty() || name == "." || name == ".." || name.contains('\\') {
            return Err(SkillError::new(
                "unsafe_path",
                format!("unsafe path component under the skills root: {name}"),
            ));
        }
        current.push(component.as_os_str());
        let metadata = match std::fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(SkillError::new(
                    "permission_denied",
                    format!("cannot inspect {}: {error}", current.display()),
                ));
            }
        };
        if is_link(&metadata) {
            return Err(SkillError::new(
                "unsafe_path",
                format!("path component must not be a link: {}", current.display()),
            ));
        }
        if index < components.len() - 1 && !metadata.is_dir() {
            return Err(SkillError::new(
                "unsafe_path",
                format!("path component must be a directory: {}", current.display()),
            ));
        }
    }
    Ok(())
}

pub(crate) fn safe_metadata(
    anchor: &Path,
    path: &Path,
) -> Result<Option<std::fs::Metadata>, SkillError> {
    ensure_safe_ancestors(anchor, path)?;
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if is_link(&metadata) {
                return Err(SkillError::new(
                    "unsafe_path",
                    format!("path must not be a link: {}", path.display()),
                ));
            }
            Ok(Some(metadata))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(SkillError::new(
            "permission_denied",
            format!("cannot inspect {}: {error}", path.display()),
        )),
    }
}

fn hash_file_bounded(path: &Path) -> Result<(u64, String), SkillError> {
    use sha2::Digest;
    let file = std::fs::File::open(path).map_err(|error| {
        SkillError::new(
            "permission_denied",
            format!("cannot read {}: {error}", path.display()),
        )
    })?;
    let mut reader = std::io::BufReader::new(file);
    let mut hasher = sha2::Sha256::new();
    let mut total: u64 = 0;
    let mut buffer = [0u8; 65536];
    loop {
        let read = reader.read(&mut buffer).map_err(|error| {
            SkillError::new(
                "permission_denied",
                format!("cannot read {}: {error}", path.display()),
            )
        })?;
        if read == 0 {
            break;
        }
        total += read as u64;
        if total > MAX_LOCAL_FILE_BYTES {
            return Err(SkillError::new(
                "integrity_error",
                format!(
                    "installed file exceeds the supported size: {}",
                    path.display()
                ),
            ));
        }
        hasher.update(&buffer[..read]);
    }
    let digest = hasher.finalize();
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    Ok((total, out))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SkillLocalState {
    Absent,
    Managed,
    Unmanaged,
    Modified,
    Unsafe,
    Error,
}

#[derive(Debug, Clone)]
pub(crate) struct FileDigest {
    pub size: u64,
    pub sha256: String,
    pub executable: bool,
}

#[derive(Debug)]
pub(crate) struct LocalSkill {
    pub state: SkillLocalState,
    pub(crate) scan_complete: bool,
    exists: bool,
    files: BTreeMap<String, FileDigest>,
    receipt: Option<InstallReceipt>,
    receipt_bytes: Option<Vec<u8>>,
    backup: Option<String>,
    error: Option<String>,
}

impl LocalSkill {
    pub(crate) fn state_name(&self) -> String {
        match self.state {
            SkillLocalState::Absent => "absent",
            SkillLocalState::Managed => "managed",
            SkillLocalState::Unmanaged => "unmanaged",
            SkillLocalState::Modified => "modified",
            SkillLocalState::Unsafe => "unsafe",
            SkillLocalState::Error => "error",
        }
        .to_string()
    }

    pub(crate) fn installed_version(&self) -> Option<String> {
        self.receipt.as_ref().map(|receipt| receipt.version.clone())
    }

    pub(crate) fn receipt_id(&self) -> Option<&str> {
        self.receipt.as_ref().map(|receipt| receipt.id.as_str())
    }

    pub(crate) fn receipt_transaction(&self) -> Option<&str> {
        self.receipt
            .as_ref()
            .map(|receipt| receipt.transaction_id.as_str())
    }

    pub(crate) fn receipt(&self) -> Option<&InstallReceipt> {
        self.receipt.as_ref()
    }

    pub(crate) fn error_message(&self) -> Option<String> {
        self.error.clone()
    }

    pub(crate) fn backup_path(&self) -> Option<String> {
        self.backup.clone()
    }

    pub(crate) fn local_revision(&self) -> String {
        let mut body = String::from("ai-cove-skill-revision-v2\n");
        body.push_str(&format!("exists\t{}\n", self.exists as u8));
        for (path, digest) in &self.files {
            body.push_str(&format!(
                "{path}\t{}\t{}\t{}\n",
                digest.size, digest.sha256, digest.executable
            ));
        }
        match &self.receipt_bytes {
            Some(bytes) => body.push_str(&format!("receipt\t{}\n", sha256_hex(bytes))),
            None => body.push_str("receipt\tnone\n"),
        }
        sha256_hex(body.as_bytes())
    }

    fn diff(
        &self,
        baseline: &BTreeMap<String, FileDigest>,
        actual: &BTreeMap<String, FileDigest>,
    ) -> Vec<(String, &'static str)> {
        let mut changes = Vec::new();
        for (path, digest) in actual {
            match baseline.get(path) {
                None => changes.push((path.clone(), "added")),
                Some(base) if digest_changed(base, digest) => {
                    changes.push((path.clone(), "modified"))
                }
                _ => {}
            }
        }
        for path in baseline.keys() {
            if !actual.contains_key(path) {
                changes.push((path.clone(), "deleted"));
            }
        }
        changes.sort_by(|a, b| a.0.cmp(&b.0));
        changes
    }

    pub(crate) fn changes_vs_receipt(&self) -> Vec<(String, &'static str)> {
        let Some(receipt) = &self.receipt else {
            return Vec::new();
        };
        let baseline: BTreeMap<String, FileDigest> = receipt
            .files
            .iter()
            .map(|file| {
                (
                    file.path.clone(),
                    FileDigest {
                        size: file.size,
                        sha256: file.sha256.clone(),
                        executable: file.executable,
                    },
                )
            })
            .collect();
        self.diff(&baseline, &self.files)
    }

    pub(crate) fn changes_vs_remote(
        &self,
        manifest: &RemoteManifest,
    ) -> Vec<(String, &'static str)> {
        let remote: BTreeMap<String, FileDigest> = manifest
            .files
            .iter()
            .map(|file| {
                (
                    file.path.clone(),
                    FileDigest {
                        size: file.size,
                        sha256: file.sha256.clone(),
                        executable: file.executable,
                    },
                )
            })
            .collect();
        self.diff(&self.files, &remote)
    }
}

fn digest_changed(a: &FileDigest, b: &FileDigest) -> bool {
    if a.size != b.size || a.sha256 != b.sha256 {
        return true;
    }
    #[cfg(unix)]
    if a.executable != b.executable {
        return true;
    }
    false
}

fn is_noise(path: &str) -> bool {
    path.split('/').any(|part| {
        part == "__pycache__"
            || part == ".DS_Store"
            || part.ends_with(".pyc")
            || part.ends_with(".pyo")
    })
}

fn check_links_in_ignored(root: &Path, rel: &str) -> Result<Option<String>, SkillError> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) => {
            return Err(SkillError::new(
                "permission_denied",
                format!("cannot scan {}: {error}", root.display()),
            ));
        }
    };
    for entry in entries {
        let entry = entry.map_err(|error| {
            SkillError::new(
                "permission_denied",
                format!("cannot scan directory entry: {error}"),
            )
        })?;
        let name = entry
            .file_name()
            .to_str()
            .map(str::to_string)
            .ok_or_else(|| SkillError::new("unsafe_path", format!("non-UTF-8 name under {rel}")))?;
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
            SkillError::new(
                "permission_denied",
                format!("cannot inspect {}: {error}", path.display()),
            )
        })?;
        if is_link(&metadata) {
            return Ok(Some(format!(
                "installed skill contains a link: {rel}/{name}"
            )));
        }
        if metadata.is_dir() {
            if let Some(flag) = check_links_in_ignored(&path, &format!("{rel}/{name}"))? {
                return Ok(Some(flag));
            }
        }
    }
    Ok(None)
}

fn scan_dir_files(
    anchor: &Path,
    root: &Path,
    prefix: &str,
    files: &mut BTreeMap<String, FileDigest>,
) -> Result<Option<String>, SkillError> {
    let mut unsafe_flag = None;
    let mut folded_names = BTreeSet::new();
    let entries = std::fs::read_dir(root).map_err(|error| {
        SkillError::new(
            "permission_denied",
            format!("cannot scan {}: {error}", root.display()),
        )
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            SkillError::new(
                "permission_denied",
                format!("cannot scan directory entry: {error}"),
            )
        })?;
        let name = entry
            .file_name()
            .to_str()
            .map(str::to_string)
            .ok_or_else(|| {
                SkillError::new("unsafe_path", format!("non-UTF-8 name under {prefix}"))
            })?;
        if !folded_names.insert(name.to_lowercase()) {
            return Ok(Some(format!(
                "case-conflicting names under {prefix}: {name}"
            )));
        }
        let rel = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
            SkillError::new(
                "permission_denied",
                format!("cannot inspect {}: {error}", path.display()),
            )
        })?;
        if is_link(&metadata) {
            unsafe_flag = Some(format!("installed skill contains a link: {rel}"));
            continue;
        }
        if metadata.is_dir() {
            if is_noise(&rel) {
                if let Some(flag) = check_links_in_ignored(&path, &rel)? {
                    unsafe_flag = Some(flag);
                }
                continue;
            }
            if let Some(flag) = scan_dir_files(anchor, &path, &rel, files)? {
                unsafe_flag = Some(flag);
            }
            continue;
        }
        if !metadata.is_file() {
            unsafe_flag = Some(format!("installed skill contains a special file: {rel}"));
            continue;
        }
        if is_noise(&rel) {
            continue;
        }
        let (size, hash) = hash_file_bounded(&path)?;
        files.insert(
            rel,
            FileDigest {
                size,
                sha256: hash,
                executable: executable_bit(&metadata),
            },
        );
    }
    Ok(unsafe_flag)
}

#[cfg(unix)]
fn executable_bit(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn executable_bit(_metadata: &std::fs::Metadata) -> bool {
    false
}

fn validate_receipt(receipt: &InstallReceipt, id: &str) -> bool {
    if receipt.schema_version != 1
        || receipt.id != id
        || receipt.source != receipt_source()
        || receipt.transaction_id.is_empty()
        || !valid_tx(&receipt.transaction_id)
        || crate::skills::parse_version(&receipt.version).is_err()
        || crate::skills_catalog::validate_sha256(&receipt.manifest_sha256).is_err()
        || receipt.files.is_empty()
    {
        return false;
    }
    if crate::skills_catalog::validate_file_paths(
        receipt.files.iter().map(|file| file.path.as_str()),
    )
    .is_err()
    {
        return false;
    }
    receipt.files.iter().all(|file| {
        file.size > 0
            && file.size <= 8 * 1024 * 1024
            && crate::skills_catalog::validate_sha256(&file.sha256).is_ok()
    }) && receipt.files.iter().map(|file| file.size).sum::<u64>() <= 32 * 1024 * 1024
        && receipt.files.iter().any(|file| file.path == "SKILL.md")
}

fn inspect_dir(anchor: &Path, dir: &Path, id: &str) -> LocalSkill {
    let mut skill = LocalSkill {
        state: SkillLocalState::Absent,
        scan_complete: false,
        exists: false,
        files: BTreeMap::new(),
        receipt: None,
        receipt_bytes: None,
        backup: None,
        error: None,
    };
    let metadata = match safe_metadata(anchor, dir) {
        Ok(Some(metadata)) => metadata,
        Ok(None) => return skill,
        Err(error) => {
            skill.state = SkillLocalState::Unsafe;
            skill.error = Some(error.message);
            return skill;
        }
    };
    if !metadata.is_dir() {
        skill.state = SkillLocalState::Unsafe;
        skill.error = Some("skill path is not a directory".to_string());
        return skill;
    }
    skill.exists = true;
    let mut files = BTreeMap::new();
    match scan_dir_files(anchor, dir, "", &mut files) {
        Ok(Some(reason)) => {
            skill.state = SkillLocalState::Unsafe;
            skill.error = Some(reason);
            return skill;
        }
        Ok(None) => {}
        Err(error) => {
            skill.state = SkillLocalState::Error;
            skill.error = Some(error.message);
            return skill;
        }
    }
    skill.scan_complete = true;
    let receipt_path = dir.join(RECEIPT_NAME);
    files.remove(RECEIPT_NAME);
    match safe_metadata(anchor, &receipt_path) {
        Ok(Some(meta)) if meta.is_file() => match std::fs::read(&receipt_path) {
            Ok(bytes) => {
                skill.receipt_bytes = Some(bytes.clone());
                match serde_json::from_slice::<InstallReceipt>(&bytes) {
                    Ok(receipt) if validate_receipt(&receipt, id) => {
                        skill.receipt = Some(receipt);
                    }
                    _ => {
                        skill.error = Some(
                            "install receipt is corrupted or does not describe this skill"
                                .to_string(),
                        );
                        skill.state = SkillLocalState::Error;
                    }
                }
            }
            Err(error) => {
                skill.error = Some(format!("cannot read install receipt: {error}"));
                skill.state = SkillLocalState::Error;
            }
        },
        Ok(Some(_)) => {
            skill.error = Some("install receipt path is unsafe".to_string());
            skill.state = SkillLocalState::Unsafe;
            return skill;
        }
        Ok(None) => {}
        Err(error) => {
            skill.error = Some(error.message);
            skill.state = SkillLocalState::Unsafe;
            return skill;
        }
    }
    skill.files = files;
    if skill.state == SkillLocalState::Absent {
        skill.state = if skill.receipt.is_some() {
            if skill.changes_vs_receipt().is_empty() {
                SkillLocalState::Managed
            } else {
                SkillLocalState::Modified
            }
        } else {
            SkillLocalState::Unmanaged
        };
    }
    skill
}

fn latest_backup(roots: &InstallRoots, id: &str) -> Option<String> {
    if validate_skill_id(id).is_err() {
        return None;
    }
    let base = roots.control_root.join("backups").join(id);
    match safe_metadata(roots.anchor(), &base) {
        Ok(Some(metadata)) if metadata.is_dir() => {}
        _ => return None,
    }
    let entries = std::fs::read_dir(&base).ok()?;
    entries
        .flatten()
        .filter(|entry| {
            let path = entry.path();
            match std::fs::symlink_metadata(&path) {
                Ok(metadata) => metadata.is_dir() && !is_link(&metadata),
                Err(_) => false,
            }
        })
        .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
        .filter(|name| valid_tx(name))
        .max()
        .map(|name| base.join(name).display().to_string())
}

pub(crate) fn inspect(roots: &InstallRoots, id: &str) -> LocalSkill {
    if validate_skill_id(id).is_err() {
        let mut skill = LocalSkill {
            state: SkillLocalState::Unsafe,
            scan_complete: false,
            exists: false,
            files: BTreeMap::new(),
            receipt: None,
            receipt_bytes: None,
            backup: None,
            error: Some(format!("invalid skill id: {id}")),
        };
        skill.backup = None;
        return skill;
    }
    let mut skill = inspect_dir(roots.anchor(), &roots.install_root.join(id), id);
    skill.backup = latest_backup(roots, id);
    skill
}

pub(crate) fn scan_installed_ids(roots: &InstallRoots) -> Result<BTreeSet<String>, SkillError> {
    let mut ids = BTreeSet::new();
    let install_root = &roots.install_root;
    if safe_metadata(roots.anchor(), install_root)?.is_none() {
        return Ok(ids);
    }
    let entries = std::fs::read_dir(install_root).map_err(|error| {
        SkillError::new(
            "permission_denied",
            format!("cannot scan installed skills: {error}"),
        )
    })?;
    for entry in entries.flatten() {
        let name = match entry.file_name().to_str() {
            Some(name) => name.to_string(),
            None => continue,
        };
        if validate_skill_id(&name).is_err() {
            continue;
        }
        let path = entry.path();
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        if is_link(&metadata) || !metadata.is_dir() {
            continue;
        }
        let receipt_path = path.join(RECEIPT_NAME);
        let metadata = match std::fs::symlink_metadata(&receipt_path) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        if is_link(&metadata) || !metadata.is_file() {
            continue;
        }
        if let Ok(file) = std::fs::File::open(&receipt_path) {
            let mut bytes = Vec::new();
            if file.take(1024 * 1024 + 1).read_to_end(&mut bytes).is_ok()
                && bytes.len() <= 1024 * 1024
            {
                if let Ok(receipt) = serde_json::from_slice::<InstallReceipt>(&bytes) {
                    if receipt.id == name && validate_receipt(&receipt, &name) {
                        ids.insert(name);
                    }
                }
            }
        }
    }
    Ok(ids)
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Journal {
    schema_version: u32,
    id: String,
    tx: String,
    operation: String,
    expected_local_revision: String,
    had_target: bool,
    release_revision: Option<String>,
}

fn valid_tx(tx: &str) -> bool {
    let parts: Vec<&str> = tx.split('-').collect();
    parts.len() == 3
        && parts[0] == "tx"
        && parts[1].chars().all(|c| c.is_ascii_digit())
        && !parts[1].is_empty()
        && parts[2].chars().all(|c| c.is_ascii_digit())
        && !parts[2].is_empty()
}

pub(crate) fn new_tx_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("tx-{nanos}-{}", std::process::id())
}

pub(crate) struct LockGuard {
    path: PathBuf,
    identity: String,
}

impl LockGuard {
    fn acquire(roots: &InstallRoots) -> Result<Self, SkillError> {
        ensure_safe_ancestors(roots.anchor(), &roots.control_root)?;
        std::fs::create_dir_all(&roots.control_root).map_err(|error| {
            SkillError::new(
                "permission_denied",
                format!("cannot create skills control root: {error}"),
            )
        })?;
        let path = roots.lock_path();
        let identity = format!("{}-{}", new_tx_id(), std::process::id());
        match std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
        {
            Ok(mut file) => {
                file.write_all(identity.as_bytes()).map_err(|error| {
                    SkillError::new(
                        "permission_denied",
                        format!("cannot write skills lock: {error}"),
                    )
                })?;
                let _ = file.sync_all();
                Ok(Self { path, identity })
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(SkillError::new(
                    "busy",
                    format!(
                        "另一个技能安装操作仍在进行，或上次操作被中断；确认没有其他 AI Cove 实例运行后，手动删除 {} 再重试",
                        path.display()
                    ),
                ))
            }
            Err(error) => Err(SkillError::new(
                "permission_denied",
                format!("cannot acquire skills lock: {error}"),
            )),
        }
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let Ok(metadata) = std::fs::symlink_metadata(&self.path) else {
            return;
        };
        if is_link(&metadata) || !metadata.is_file() || metadata.len() > 4096 {
            return;
        }
        if let Ok(file) = std::fs::File::open(&self.path) {
            let mut bytes = Vec::new();
            if file.take(4097).read_to_end(&mut bytes).is_ok() && bytes == self.identity.as_bytes()
            {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }
}

fn write_journal(roots: &InstallRoots, journal: &Journal) -> Result<(), SkillError> {
    if roots.failpoints.fail_journal_write {
        return Err(SkillError::new(
            "internal",
            "injected journal write failure",
        ));
    }
    let dir = roots.transactions_dir();
    ensure_safe_ancestors(roots.anchor(), &dir)?;
    std::fs::create_dir_all(&dir).map_err(|error| {
        SkillError::new(
            "permission_denied",
            format!("cannot create transaction directory: {error}"),
        )
    })?;
    ensure_safe_ancestors(roots.anchor(), &dir)?;
    let target = roots.journal_path(&journal.tx);
    ensure_safe_ancestors(roots.anchor(), &target)?;
    if safe_metadata(roots.anchor(), &target)?.is_some() {
        return Err(SkillError::new(
            "unsafe_path",
            "transaction journal path already exists",
        ));
    }
    let data = serde_json::to_vec(journal)
        .map_err(|error| SkillError::new("internal", format!("journal encode failed: {error}")))?;
    let mut tmp = tempfile::NamedTempFile::new_in(&dir).map_err(|error| {
        SkillError::new(
            "permission_denied",
            format!("cannot write transaction journal: {error}"),
        )
    })?;
    tmp.write_all(&data)
        .and_then(|_| tmp.as_file().sync_all())
        .map_err(|error| {
            SkillError::new(
                "permission_denied",
                format!("cannot write transaction journal: {error}"),
            )
        })?;
    tmp.persist(&target)
        .map_err(|_| SkillError::new("permission_denied", "cannot commit transaction journal"))?;
    sync_dir(&dir);
    Ok(())
}

fn sync_dir(dir: &Path) {
    if let Ok(dir_file) = std::fs::File::open(dir) {
        let _ = dir_file.sync_all();
    }
}

fn remove_journal(roots: &InstallRoots, tx: &str) {
    let _ = std::fs::remove_file(roots.journal_path(tx));
}

fn revision_of(anchor: &Path, dir: &Path, id: &str) -> String {
    inspect_dir(anchor, dir, id).local_revision()
}

fn recovery_error() -> SkillError {
    SkillError::new(
        "internal",
        "上次安装中断，安装目录状态无法自动确认；请检查安装目录后重试",
    )
}

fn valid_revision(value: &str) -> bool {
    crate::skills_catalog::validate_sha256(value).is_ok()
}

fn recover_pending(roots: &InstallRoots, keep_tx: &str) -> Result<(), SkillError> {
    let dir = roots.transactions_dir();
    if safe_metadata(roots.anchor(), &dir)?.is_none() {
        return Ok(());
    }
    let entries = std::fs::read_dir(&dir).map_err(|error| {
        SkillError::new(
            "permission_denied",
            format!("cannot scan transaction journals: {error}"),
        )
    })?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let stem = match path.file_stem().and_then(|stem| stem.to_str()) {
            Some(stem) => stem.to_string(),
            None => continue,
        };
        if stem == keep_tx {
            continue;
        }
        if !valid_tx(&stem) {
            continue;
        }
        let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
            SkillError::new(
                "permission_denied",
                format!("cannot inspect journal: {error}"),
            )
        })?;
        if is_link(&metadata) || !metadata.is_file() {
            return Err(SkillError::new(
                "unsafe_path",
                "transaction journal must be a regular file",
            ));
        }
        let file = std::fs::File::open(&path).map_err(|error| {
            SkillError::new("permission_denied", format!("cannot read journal: {error}"))
        })?;
        let mut bytes = Vec::new();
        file.take(64 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| {
                SkillError::new("permission_denied", format!("cannot read journal: {error}"))
            })?;
        if bytes.len() > 64 * 1024 {
            return Err(recovery_error());
        }
        let journal: Journal = serde_json::from_slice(&bytes).map_err(|_| recovery_error())?;
        if journal.schema_version != 1
            || journal.tx != stem
            || validate_skill_id(&journal.id).is_err()
            || !valid_tx(&journal.tx)
            || !valid_revision(&journal.expected_local_revision)
        {
            return Err(recovery_error());
        }
        let target = roots.install_root.join(&journal.id);
        let backup = roots.backup_dir(&journal.id, &journal.tx);
        let staging = roots.staging_dir(&journal.tx);
        let anchor = roots.anchor().to_path_buf();
        let clean_owned_stage = |roots: &InstallRoots, staging: &Path| -> Result<(), SkillError> {
            if safe_metadata(roots.anchor(), staging)?.is_some() {
                std::fs::remove_dir_all(staging).map_err(|error| {
                    SkillError::new(
                        "permission_denied",
                        format!("cannot clean staged skill: {error}"),
                    )
                })?;
            }
            Ok(())
        };
        match journal.operation.as_str() {
            "install" => {
                let release_revision = match journal.release_revision.as_deref() {
                    Some(revision) if valid_revision(revision) => revision,
                    _ => return Err(recovery_error()),
                };
                let target_local = inspect_dir(&anchor, &target, &journal.id);
                match target_local.state {
                    SkillLocalState::Unsafe | SkillLocalState::Error => {
                        return Err(recovery_error());
                    }
                    _ => {}
                }
                let committed = target_local.state == SkillLocalState::Managed
                    && target_local
                        .receipt_transaction()
                        .map(|tx| tx == journal.tx)
                        .unwrap_or(false)
                    && target_local
                        .receipt()
                        .map(|receipt| receipt.manifest_sha256 == release_revision)
                        .unwrap_or(false);
                if committed {
                    clean_owned_stage(roots, &staging)?;
                    remove_journal(roots, &journal.tx);
                    continue;
                }
                if !target_local.exists {
                    if !journal.had_target {
                        clean_owned_stage(roots, &staging)?;
                        remove_journal(roots, &journal.tx);
                        continue;
                    }
                    match safe_metadata(&anchor, &backup)? {
                        Some(metadata) if metadata.is_dir() => {
                            if revision_of(&anchor, &backup, &journal.id)
                                != journal.expected_local_revision
                            {
                                return Err(recovery_error());
                            }
                            std::fs::rename(&backup, &target).map_err(|error| {
                                SkillError::new(
                                    "internal",
                                    format!("上次安装中断且无法恢复 {}: {error}", target.display()),
                                )
                            })?;
                            sync_dir(&roots.install_root);
                            clean_owned_stage(roots, &staging)?;
                            remove_journal(roots, &journal.tx);
                            continue;
                        }
                        Some(_) => return Err(recovery_error()),
                        None => return Err(recovery_error()),
                    }
                }
                if target_local.local_revision() == journal.expected_local_revision
                    && safe_metadata(&anchor, &backup)?.is_none()
                {
                    clean_owned_stage(roots, &staging)?;
                    remove_journal(roots, &journal.tx);
                    continue;
                }
                return Err(recovery_error());
            }
            "uninstall" => {
                let target_local = inspect_dir(&anchor, &target, &journal.id);
                match target_local.state {
                    SkillLocalState::Unsafe | SkillLocalState::Error => {
                        return Err(recovery_error());
                    }
                    _ => {}
                }
                if !target_local.exists {
                    match safe_metadata(&anchor, &backup)? {
                        Some(metadata) if metadata.is_dir() => {
                            if revision_of(&anchor, &backup, &journal.id)
                                != journal.expected_local_revision
                            {
                                return Err(recovery_error());
                            }
                            remove_journal(roots, &journal.tx);
                            continue;
                        }
                        _ => return Err(recovery_error()),
                    }
                }
                if target_local.local_revision() == journal.expected_local_revision
                    && safe_metadata(&anchor, &backup)?.is_none()
                {
                    remove_journal(roots, &journal.tx);
                    continue;
                }
                return Err(recovery_error());
            }
            _ => return Err(recovery_error()),
        }
    }
    Ok(())
}

pub(crate) fn begin_mutation(roots: &InstallRoots) -> Result<LockGuard, SkillError> {
    let guard = LockGuard::acquire(roots)?;
    recover_pending(roots, "")?;
    Ok(guard)
}

fn receipt_for(manifest: &RemoteManifest, tx: &str) -> Result<Vec<u8>, SkillError> {
    let receipt = InstallReceipt {
        schema_version: 1,
        source: receipt_source().to_string(),
        id: manifest.id.clone(),
        version: manifest.version.clone(),
        manifest_sha256: manifest.sha256.clone(),
        files: manifest
            .files
            .iter()
            .map(|file| ReceiptFile {
                path: file.path.clone(),
                size: file.size,
                sha256: file.sha256.clone(),
                executable: file.executable,
            })
            .collect(),
        installed_at_unix_seconds: unix_seconds_now(),
        transaction_id: tx.to_string(),
    };
    serde_json::to_vec_pretty(&receipt)
        .map_err(|error| SkillError::new("internal", format!("receipt encode failed: {error}")))
}

fn verify_staged(
    roots: &InstallRoots,
    staging: &Path,
    manifest: &RemoteManifest,
) -> Result<(), SkillError> {
    let anchor = roots.anchor();
    let mut found = BTreeSet::new();
    for file in &manifest.files {
        let rel = file
            .path
            .split('/')
            .fold(staging.to_path_buf(), |mut path, part| {
                path.push(part);
                path
            });
        let metadata = safe_metadata(anchor, &rel)?.ok_or_else(|| {
            SkillError::new(
                "integrity_error",
                format!("staged file missing: {}", file.path),
            )
        })?;
        if !metadata.is_file() {
            return Err(SkillError::new(
                "integrity_error",
                format!("staged path is not a file: {}", file.path),
            ));
        }
        let (size, hash) = hash_file_bounded(&rel)?;
        if size != file.size || hash != file.sha256 {
            return Err(SkillError::new(
                "integrity_error",
                format!("staged file checksum mismatch: {}", file.path),
            ));
        }
        found.insert(file.path.clone());
    }
    let mut extra = Vec::new();
    let mut stack = vec![(staging.to_path_buf(), String::new())];
    while let Some((dir, prefix)) = stack.pop() {
        let entries = std::fs::read_dir(&dir).map_err(|error| {
            SkillError::new(
                "permission_denied",
                format!("cannot scan staged skill: {error}"),
            )
        })?;
        for entry in entries.flatten() {
            let name = entry
                .file_name()
                .to_str()
                .map(str::to_string)
                .ok_or_else(|| SkillError::new("unsafe_path", "non-UTF-8 staged file name"))?;
            let rel = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
                SkillError::new(
                    "permission_denied",
                    format!("cannot inspect staged path: {error}"),
                )
            })?;
            if is_link(&metadata) {
                return Err(SkillError::new(
                    "unsafe_path",
                    format!("staged path is a link: {rel}"),
                ));
            }
            if metadata.is_dir() {
                stack.push((path, rel));
            } else if rel != RECEIPT_NAME && !found.contains(&rel) {
                extra.push(rel);
            }
        }
    }
    if !extra.is_empty() {
        return Err(SkillError::new(
            "integrity_error",
            format!(
                "staged skill contains undeclared files: {}",
                extra.join(", ")
            ),
        ));
    }
    let skill_md = staging.join("SKILL.md");
    let (size, _hash) = hash_file_bounded(&skill_md)
        .map_err(|_| SkillError::new("integrity_error", "staged skill is missing SKILL.md"))?;
    if size == 0 {
        return Err(SkillError::new(
            "integrity_error",
            "staged SKILL.md is empty",
        ));
    }
    let bytes = std::fs::read(&skill_md).map_err(|error| {
        SkillError::new(
            "permission_denied",
            format!("cannot read staged SKILL.md: {error}"),
        )
    })?;
    let text = String::from_utf8(bytes)
        .map_err(|_| SkillError::new("integrity_error", "staged SKILL.md is not UTF-8"))?;
    let lines: Vec<&str> = text.lines().collect();
    if lines.first().map(|line| line.trim()) != Some("---") {
        return Err(SkillError::new(
            "integrity_error",
            "staged SKILL.md lacks frontmatter",
        ));
    }
    let mut name = None;
    let mut closed = false;
    for line in &lines[1..] {
        if line.trim() == "---" {
            closed = true;
            break;
        }
        if let Some(rest) = line.strip_prefix("name:") {
            if name.is_some() {
                return Err(SkillError::new(
                    "integrity_error",
                    "duplicate SKILL.md name",
                ));
            }
            name = Some(rest.trim().to_string());
        }
    }
    if !closed || name.as_deref() != Some(manifest.id.as_str()) {
        return Err(SkillError::new(
            "integrity_error",
            "staged SKILL.md name does not match the skill id",
        ));
    }
    Ok(())
}

fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    let pa = crate::skills::parse_version(a);
    let pb = crate::skills::parse_version(b);
    match (pa, pb) {
        (Ok(a), Ok(b)) => a.cmp(&b),
        (Err(_), Ok(_)) => std::cmp::Ordering::Less,
        (Ok(_), Err(_)) => std::cmp::Ordering::Greater,
        (Err(_), Err(_)) => a.cmp(b),
    }
}

fn restore_backup(roots: &InstallRoots, target: &Path, backup: &Path) -> Result<(), SkillError> {
    if safe_metadata(roots.anchor(), target)?.is_some()
        || !safe_metadata(roots.anchor(), backup)?.is_some_and(|metadata| metadata.is_dir())
    {
        return Err(SkillError::new(
            "internal",
            format!("恢复目录状态不明确；请保留并检查备份 {}", backup.display()),
        ));
    }
    std::fs::rename(backup, target).map_err(|error| {
        SkillError::new(
            "internal",
            format!(
                "无法恢复之前的安装目录到 {}（备份仍在 {}）：{error}",
                target.display(),
                backup.display()
            ),
        )
    })?;
    sync_dir(&roots.install_root);
    Ok(())
}

pub(crate) async fn install_staged(
    roots: &InstallRoots,
    _guard: &LockGuard,
    tx: &str,
    manifest: &RemoteManifest,
    expected_local_revision: &str,
    confirm_replace: bool,
) -> Result<Option<PathBuf>, SkillError> {
    if !valid_tx(tx) {
        return Err(SkillError::new("internal", "invalid transaction id"));
    }
    let id = manifest.id.clone();
    validate_skill_id(&id)?;
    let staging = roots.staging_dir(tx);
    verify_staged(roots, &staging, manifest)?;
    let local = inspect(roots, &id);
    if local.local_revision() != expected_local_revision {
        return Err(SkillError::new(
            "revision_changed",
            "本地内容在上次检查后发生变化；请重新查看差异再操作",
        ));
    }
    match local.state {
        SkillLocalState::Absent | SkillLocalState::Managed => {}
        SkillLocalState::Modified | SkillLocalState::Unmanaged => {
            if !confirm_replace {
                return Err(SkillError::new(
                    "local_conflict",
                    "本地存在修改或未登记的同名目录，需要确认后才会备份并替换",
                ));
            }
        }
        SkillLocalState::Error => {
            if !local.scan_complete {
                return Err(SkillError::new(
                    "unsafe_path",
                    local
                        .error
                        .unwrap_or_else(|| "无法读取本地技能目录".to_string()),
                ));
            }
            if !confirm_replace {
                return Err(SkillError::new(
                    "local_conflict",
                    "本地存在修改或未登记的同名目录，需要确认后才会备份并替换",
                ));
            }
        }
        SkillLocalState::Unsafe => {
            return Err(SkillError::new(
                "unsafe_path",
                local
                    .error
                    .unwrap_or_else(|| "skill path is unsafe".to_string()),
            ));
        }
    }
    if let Some(version) = local.installed_version() {
        if compare_versions(&version, &manifest.version) == std::cmp::Ordering::Greater {
            return Err(SkillError::new(
                "local_conflict",
                "本地安装版本高于官方目录，不会自动降级",
            ));
        }
    }
    let anchor = roots.anchor().to_path_buf();
    ensure_safe_ancestors(&anchor, &roots.install_root)?;
    std::fs::create_dir_all(&roots.install_root).map_err(|error| {
        SkillError::new(
            "permission_denied",
            format!("cannot create install root: {error}"),
        )
    })?;
    ensure_safe_ancestors(&anchor, &roots.install_root)?;
    let target = roots.install_root.join(&id);
    ensure_safe_ancestors(&anchor, &target)?;
    let receipt = receipt_for(manifest, tx)?;
    let receipt_path = staging.join(RECEIPT_NAME);
    {
        let mut file = std::fs::File::create(&receipt_path).map_err(|error| {
            SkillError::new(
                "permission_denied",
                format!("cannot write install receipt: {error}"),
            )
        })?;
        file.write_all(&receipt)
            .and_then(|_| file.sync_all())
            .map_err(|error| {
                SkillError::new(
                    "permission_denied",
                    format!("cannot write install receipt: {error}"),
                )
            })?;
    }
    let backup = roots.backup_dir(&id, tx);
    let journal = Journal {
        schema_version: 1,
        id: id.clone(),
        tx: tx.to_string(),
        operation: "install".to_string(),
        expected_local_revision: expected_local_revision.to_string(),
        had_target: local.exists,
        release_revision: Some(manifest.sha256.clone()),
    };
    write_journal(roots, &journal)?;
    let recheck = inspect(roots, &id);
    if recheck.local_revision() != expected_local_revision {
        remove_journal(roots, tx);
        return Err(SkillError::new(
            "revision_changed",
            "本地内容在上次检查后发生变化；请重新查看差异再操作",
        ));
    }
    let mut backup_path = None;
    if local.exists {
        ensure_safe_ancestors(&anchor, &backup)?;
        if let Some(parent) = backup.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                SkillError::new(
                    "permission_denied",
                    format!("cannot create backup directory: {error}"),
                )
            })?;
            ensure_safe_ancestors(&anchor, &backup)?;
        }
        std::fs::rename(&target, &backup).map_err(|error| {
            SkillError::new(
                "permission_denied",
                format!("cannot back up existing skill: {error}"),
            )
        })?;
        if let Some(parent) = backup.parent() {
            sync_dir(parent);
        }
        backup_path = Some(backup.clone());
    }
    if roots.failpoints.fail_after_old_rename {
        return Err(SkillError::new("internal", "injected post-rename failure"));
    }
    let fail_activation = roots.failpoints.fail_activation;
    let activate = || -> Result<(), SkillError> {
        if fail_activation {
            return Err(SkillError::new("internal", "injected activation failure"));
        }
        std::fs::rename(&staging, &target).map_err(|error| {
            SkillError::new(
                "permission_denied",
                format!("cannot activate staged skill: {error}"),
            )
        })?;
        sync_dir(&roots.install_root);
        Ok(())
    };
    if let Err(error) = activate() {
        if let Some(backup) = &backup_path {
            restore_backup(roots, &target, backup)?;
        }
        remove_journal(roots, tx);
        return Err(error);
    }
    let verified = inspect(roots, &id);
    let committed = !roots.failpoints.fail_post_verify
        && verified.state == SkillLocalState::Managed
        && verified.receipt_transaction() == Some(tx)
        && verified
            .receipt()
            .map(|receipt| receipt.manifest_sha256 == manifest.sha256)
            .unwrap_or(false);
    if !committed {
        if verified.receipt_transaction() != Some(tx) {
            return Err(SkillError::new(
                "internal",
                "安装后的目录被外部修改，已保留目录和备份，请检查后再恢复",
            ));
        }
        let quarantine = roots.quarantine_dir(tx);
        ensure_safe_ancestors(&anchor, &quarantine)?;
        if safe_metadata(&anchor, &quarantine)?.is_some() {
            return Err(recovery_error());
        }
        let parent = quarantine.parent().ok_or_else(recovery_error)?;
        std::fs::create_dir_all(parent)
            .map_err(|error| SkillError::new("permission_denied", error.to_string()))?;
        ensure_safe_ancestors(&anchor, &quarantine)?;
        std::fs::rename(&target, &quarantine).map_err(|error| {
            SkillError::new(
                "internal",
                format!("无法隔离未通过校验的安装，原目录和备份已保留：{error}"),
            )
        })?;
        sync_dir(parent);
        if let Some(backup) = &backup_path {
            restore_backup(roots, &target, backup)?;
        }
        remove_journal(roots, tx);
        return Err(SkillError::new(
            "integrity_error",
            "安装校验失败，已回退安装目录并保留未通过校验的文件",
        ));
    }
    remove_journal(roots, tx);
    Ok(backup_path)
}

pub(crate) async fn uninstall(
    roots: &InstallRoots,
    _guard: &LockGuard,
    id: &str,
    expected_local_revision: &str,
) -> Result<PathBuf, SkillError> {
    validate_skill_id(id)?;
    let local = inspect(roots, id);
    if local.state == SkillLocalState::Absent {
        return Err(SkillError::new(
            "not_found",
            format!("skill {id} is not installed"),
        ));
    }
    if local.state == SkillLocalState::Unsafe
        || (local.state == SkillLocalState::Error && !local.scan_complete)
    {
        return Err(SkillError::new(
            "unsafe_path",
            local
                .error
                .unwrap_or_else(|| "skill path is unsafe".to_string()),
        ));
    }
    if local.local_revision() != expected_local_revision {
        return Err(SkillError::new(
            "revision_changed",
            "本地内容在上次检查后发生变化；请重新查看差异再操作",
        ));
    }
    let tx = new_tx_id();
    let target = roots.install_root.join(id);
    let backup = roots.backup_dir(id, &tx);
    let anchor = roots.anchor().to_path_buf();
    let journal = Journal {
        schema_version: 1,
        id: id.to_string(),
        tx: tx.clone(),
        operation: "uninstall".to_string(),
        expected_local_revision: expected_local_revision.to_string(),
        had_target: true,
        release_revision: None,
    };
    write_journal(roots, &journal)?;
    let recheck = inspect(roots, id);
    if recheck.local_revision() != expected_local_revision {
        remove_journal(roots, &tx);
        return Err(SkillError::new(
            "revision_changed",
            "本地内容在上次检查后发生变化；请重新查看差异再操作",
        ));
    }
    ensure_safe_ancestors(&anchor, &backup)?;
    if let Some(parent) = backup.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            SkillError::new(
                "permission_denied",
                format!("cannot create backup directory: {error}"),
            )
        })?;
        ensure_safe_ancestors(&anchor, &backup)?;
    }
    if roots.failpoints.fail_backup_move {
        return Err(SkillError::new("internal", "injected backup move failure"));
    }
    std::fs::rename(&target, &backup).map_err(|error| {
        SkillError::new(
            "permission_denied",
            format!("cannot move skill into backup: {error}"),
        )
    })?;
    if let Some(parent) = backup.parent() {
        sync_dir(parent);
    }
    if revision_of(&anchor, &backup, id) != expected_local_revision {
        restore_backup(roots, &target, &backup)?;
        return Err(SkillError::new(
            "internal",
            "卸载备份内容校验失败；目录已恢复原位",
        ));
    }
    remove_journal(roots, &tx);
    Ok(backup)
}
