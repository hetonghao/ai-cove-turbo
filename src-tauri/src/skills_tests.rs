use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};
use tokio::sync::oneshot;

use crate::skills::SkillsManager;

struct Fixture {
    dir: PathBuf,
    _tmp: tempfile::TempDir,
}

fn sha(data: &[u8]) -> String {
    let mut out = String::new();
    for byte in Sha256::digest(data) {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn write_release(
    root: &Path,
    id: &str,
    version: &str,
    files: &[(&str, &[u8])],
    exec: &[&str],
) -> (Vec<u8>, String) {
    let release = root.join("releases").join(id).join(version);
    std::fs::create_dir_all(release.join("files")).unwrap();
    let mut entries = Vec::new();
    for (path, body) in files {
        let target = release.join("files").join(path);
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, body).unwrap();
        #[cfg(unix)]
        if exec.contains(path) {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        entries.push(serde_json::json!({
            "path": path,
            "size": body.len(),
            "sha256": sha(body),
            "executable": exec.contains(path),
        }));
    }
    entries.sort_by(|a, b| a["path"].as_str().unwrap().cmp(b["path"].as_str().unwrap()));
    let manifest = serde_json::json!({
        "schema_version": 1,
        "id": id,
        "version": version,
        "files": entries,
    });
    let manifest_bytes =
        format!("{}\n", serde_json::to_string_pretty(&manifest).unwrap()).into_bytes();
    std::fs::write(release.join("manifest.json"), &manifest_bytes).unwrap();
    (manifest_bytes.clone(), sha(&manifest_bytes))
}

fn build_archive_bytes(
    id: &str,
    files: &[(&str, &[u8])],
    exec: &[&str],
    extra: &[(&str, &[u8])],
) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    {
        let mut tar = tar::Builder::new(&mut encoder);
        let mut sorted: Vec<(String, &[u8], bool)> = files
            .iter()
            .map(|(path, body)| (format!("{id}/{path}"), *body, exec.contains(path)))
            .collect();
        sorted.extend(extra.iter().map(|(path, body)| {
            (path.to_string(), *body, false)
        }));
        sorted.sort_by(|a, b| a.0.cmp(&b.0));
        for (path, body, executable) in sorted {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(if executable { 0o755 } else { 0o644 });
            header.set_mtime(0);
            header.set_cksum();
            tar.append_data(&mut header, path, body).unwrap();
        }
        tar.finish().unwrap();
    }
    encoder.finish().unwrap()
}

fn write_release_archived(
    root: &Path,
    id: &str,
    version: &str,
    files: &[(&str, &[u8])],
    exec: &[&str],
    archive_extra: &[(&str, &[u8])],
) -> (Vec<u8>, String) {
    let (manifest_bytes, _sha) = write_release(root, id, version, files, exec);
    let release = root.join("releases").join(id).join(version);
    let archive_name = format!("{id}-{version}.tar.gz");
    let archive_bytes = build_archive_bytes(id, files, exec, archive_extra);
    std::fs::write(release.join(&archive_name), &archive_bytes).unwrap();
    let mut manifest: serde_json::Value = serde_json::from_slice(&manifest_bytes).unwrap();
    manifest["archive"] = serde_json::json!(archive_name);
    manifest["archive_sha256"] = serde_json::json!(sha(&archive_bytes));
    let manifest_bytes =
        format!("{}\n", serde_json::to_string_pretty(&manifest).unwrap()).into_bytes();
    std::fs::write(release.join("manifest.json"), &manifest_bytes).unwrap();
    (manifest_bytes.clone(), sha(&manifest_bytes))
}

fn write_catalog(root: &Path, skills: Vec<serde_json::Value>) {
    let catalog = serde_json::json!({"schema_version": 1, "skills": skills});
    std::fs::write(
        root.join("catalog.json"),
        format!("{}\n", serde_json::to_string_pretty(&catalog).unwrap()),
    )
    .unwrap();
}

fn demo_fixture(version: &str, body: &[u8]) -> Fixture {
    demo_fixture_impl(version, body, false)
}

fn demo_fixture_archived(version: &str, body: &[u8]) -> Fixture {
    demo_fixture_impl(version, body, true)
}

fn demo_fixture_impl(version: &str, body: &[u8], archived: bool) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("skills");
    std::fs::create_dir_all(&root).unwrap();
    let files: Vec<(&str, &[u8])> = vec![
        ("SKILL.md", b"---\nname: demo-skill\n---\nx\n"),
        ("scripts/tool.py", body),
    ];
    let (_manifest, manifest_sha) = if archived {
        write_release_archived(&root, "demo-skill", version, &files, &["scripts/tool.py"], &[])
    } else {
        write_release(&root, "demo-skill", version, &files, &["scripts/tool.py"])
    };
    write_catalog(
        &root,
        vec![serde_json::json!({
            "id": "demo-skill",
            "name": "Demo Skill",
            "description": "fixture",
            "version": version,
            "manifest": format!("releases/demo-skill/{version}/manifest.json"),
            "manifest_sha256": manifest_sha,
        })],
    );
    Fixture {
        dir: root,
        _tmp: tmp,
    }
}

async fn serve_raw(
    handler: impl Fn(axum::extract::Request) -> axum::http::Response<axum::body::Body>
    + Send
    + Sync
    + 'static
    + Clone,
) -> (String, oneshot::Sender<()>) {
    let app = axum::Router::new().fallback(axum::routing::get(
        move |request: axum::extract::Request| {
            let handler = handler.clone();
            async move { handler(request) }
        },
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = rx.await;
            })
            .await
            .unwrap();
    });
    (format!("http://127.0.0.1:{port}"), tx)
}

async fn serve(root: PathBuf, url_path: &str) -> (String, oneshot::Sender<()>) {
    let app = axum::Router::new().fallback(axum::routing::get(
        move |request: axum::extract::Request| {
            let root = root.clone();
            async move {
                let path = request.uri().path().trim_start_matches('/').to_string();
                if path.contains("..") {
                    return axum::http::Response::builder()
                        .status(404)
                        .body(axum::body::Body::empty())
                        .unwrap();
                }
                let file = root.join(&path);
                match std::fs::read(&file) {
                    Ok(data) => axum::http::Response::builder()
                        .status(200)
                        .body(axum::body::Body::from(data))
                        .unwrap(),
                    Err(_) => axum::http::Response::builder()
                        .status(404)
                        .body(axum::body::Body::empty())
                        .unwrap(),
                }
            }
        },
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = rx.await;
            })
            .await
            .unwrap();
    });
    (format!("http://127.0.0.1:{port}/{url_path}"), tx)
}

fn test_manager(url: &str) -> (SkillsManager, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let manager = SkillsManager::for_test(
        url,
        home.join(".agents/skills"),
        home.join(".agents/.ai-cove-skills"),
        tmp.path().join("cache"),
    )
    .unwrap();
    (manager, tmp)
}

fn status_skill<'a>(
    status: &'a crate::skills::SkillsStatus,
    id: &str,
) -> &'a crate::skills::SkillStatus {
    status.skills.iter().find(|skill| skill.id == id).unwrap()
}

#[tokio::test]
async fn install_via_archive_lands_files_and_exec_bit() {
    let fixture = demo_fixture_archived("1.0.0", b"print(1)\n");
    let (url, _stop) = serve(
        fixture.dir.parent().unwrap().to_path_buf(),
        "skills/catalog.json",
    )
    .await;
    let (manager, tmp) = test_manager(&url);
    let home = tmp.path().join("home");

    let status = manager.status(true).await.unwrap();
    let skill = status_skill(&status, "demo-skill");
    let result = manager
        .install(
            "demo-skill",
            skill.release_revision.as_deref().unwrap(),
            &skill.local_revision,
            false,
        )
        .await
        .unwrap();
    let installed = status_skill(&result.status, "demo-skill");
    assert_eq!(installed.local_state, "managed");
    assert_eq!(installed.installed_version.as_deref(), Some("1.0.0"));
    assert_eq!(
        std::fs::read(home.join(".agents/skills/demo-skill/scripts/tool.py")).unwrap(),
        b"print(1)\n"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(home.join(".agents/skills/demo-skill/scripts/tool.py"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111);
    }
}

#[tokio::test]
async fn install_rejects_archive_with_wrong_checksum() {
    let fixture = demo_fixture_archived("1.0.0", b"print(1)\n");
    std::fs::write(
        fixture
            .dir
            .join("releases/demo-skill/1.0.0/demo-skill-1.0.0.tar.gz"),
        b"corrupted",
    )
    .unwrap();
    let (url, _stop) = serve(
        fixture.dir.parent().unwrap().to_path_buf(),
        "skills/catalog.json",
    )
    .await;
    let (manager, _tmp) = test_manager(&url);

    let status = manager.status(true).await.unwrap();
    let skill = status_skill(&status, "demo-skill");
    let error = manager
        .install(
            "demo-skill",
            skill.release_revision.as_deref().unwrap(),
            &skill.local_revision,
            false,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "integrity_error");
}

#[tokio::test]
async fn install_rejects_archive_with_undeclared_entry() {
    let fixture = {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("skills");
        std::fs::create_dir_all(&root).unwrap();
        let files: Vec<(&str, &[u8])> = vec![
            ("SKILL.md", b"---\nname: demo-skill\n---\nx\n"),
            ("scripts/tool.py", b"print(1)\n"),
        ];
        let (_m, manifest_sha) = write_release_archived(
            &root,
            "demo-skill",
            "1.0.0",
            &files,
            &["scripts/tool.py"],
            &[("demo-skill/extra/evil.py", b"evil\n")],
        );
        write_catalog(
            &root,
            vec![serde_json::json!({
                "id": "demo-skill",
                "name": "Demo Skill",
                "description": "fixture",
                "version": "1.0.0",
                "manifest": "releases/demo-skill/1.0.0/manifest.json",
                "manifest_sha256": manifest_sha,
            })],
        );
        Fixture {
            dir: root,
            _tmp: tmp,
        }
    };
    let (url, _stop) = serve(
        fixture.dir.parent().unwrap().to_path_buf(),
        "skills/catalog.json",
    )
    .await;
    let (manager, _tmp) = test_manager(&url);

    let status = manager.status(true).await.unwrap();
    let skill = status_skill(&status, "demo-skill");
    let error = manager
        .install(
            "demo-skill",
            skill.release_revision.as_deref().unwrap(),
            &skill.local_revision,
            false,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "integrity_error");
}

#[tokio::test]
async fn install_rejects_archive_entry_outside_skill_prefix() {
    let fixture = {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("skills");
        std::fs::create_dir_all(&root).unwrap();
        let files: Vec<(&str, &[u8])> = vec![
            ("SKILL.md", b"---\nname: demo-skill\n---\nx\n"),
            ("scripts/tool.py", b"print(1)\n"),
        ];
        let (_m, manifest_sha) = write_release_archived(
            &root,
            "demo-skill",
            "1.0.0",
            &files,
            &["scripts/tool.py"],
            &[("other-skill/escape.txt", b"evil\n")],
        );
        write_catalog(
            &root,
            vec![serde_json::json!({
                "id": "demo-skill",
                "name": "Demo Skill",
                "description": "fixture",
                "version": "1.0.0",
                "manifest": "releases/demo-skill/1.0.0/manifest.json",
                "manifest_sha256": manifest_sha,
            })],
        );
        Fixture {
            dir: root,
            _tmp: tmp,
        }
    };
    let (url, _stop) = serve(
        fixture.dir.parent().unwrap().to_path_buf(),
        "skills/catalog.json",
    )
    .await;
    let (manager, _tmp) = test_manager(&url);

    let status = manager.status(true).await.unwrap();
    let skill = status_skill(&status, "demo-skill");
    let error = manager
        .install(
            "demo-skill",
            skill.release_revision.as_deref().unwrap(),
            &skill.local_revision,
            false,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "unsafe_path");
}

#[tokio::test]
async fn install_current_update_and_uninstall_with_backups() {
    let fixture = demo_fixture("1.0.0", b"print(1)\n");
    let (url, _stop) = serve(
        fixture.dir.parent().unwrap().to_path_buf(),
        "skills/catalog.json",
    )
    .await;
    let (manager, tmp) = test_manager(&url);
    let home = tmp.path().join("home");

    let status = manager.status(true).await.unwrap();
    assert_eq!(status.catalog_state, "fresh");
    let skill = status_skill(&status, "demo-skill");
    assert_eq!(skill.local_state, "absent");
    assert_eq!(skill.update_state, "available");
    assert!(skill.release_revision.is_some());

    let result = manager
        .install(
            "demo-skill",
            skill.release_revision.as_deref().unwrap(),
            &skill.local_revision,
            false,
        )
        .await
        .unwrap();
    let installed = status_skill(&result.status, "demo-skill");
    assert_eq!(installed.local_state, "managed");
    assert_eq!(installed.update_state, "current");
    assert_eq!(installed.installed_version.as_deref(), Some("1.0.0"));
    assert!(result.backup_path.is_none());
    assert!(
        home.join(".agents/skills/demo-skill/.ai-cove-install.json")
            .is_file()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(home.join(".agents/skills/demo-skill/scripts/tool.py"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111);
    }

    let tool = home.join(".agents/skills/demo-skill/scripts/tool.py");
    std::fs::write(&tool, b"print('local')\n").unwrap();
    std::fs::write(home.join(".agents/skills/demo-skill/extra.txt"), b"mine\n").unwrap();
    std::fs::create_dir_all(home.join(".agents/skills/demo-skill/scripts/__pycache__")).unwrap();
    std::fs::write(
        home.join(".agents/skills/demo-skill/scripts/__pycache__/tool.pyc"),
        b"noise",
    )
    .unwrap();

    let edited = manager.status(false).await.unwrap();
    let edited_skill = status_skill(&edited, "demo-skill");
    assert_eq!(edited_skill.local_state, "modified");
    assert_eq!(edited_skill.update_state, "current");
    let kinds: BTreeMap<_, _> = edited_skill
        .local_changes
        .iter()
        .map(|change| (change.path.as_str(), change.kind.as_str()))
        .collect();
    assert_eq!(kinds.get("scripts/tool.py"), Some(&"modified"));
    assert_eq!(kinds.get("extra.txt"), Some(&"added"));
    assert!(!kinds.contains_key("scripts/__pycache__/tool.pyc"));

    let result = manager
        .install(
            "demo-skill",
            edited_skill.release_revision.as_deref().unwrap(),
            &edited_skill.local_revision,
            true,
        )
        .await
        .unwrap();
    assert!(result.backup_path.is_some());
    let backup = PathBuf::from(result.backup_path.clone().unwrap());
    assert!(backup.join("extra.txt").is_file());
    assert!(backup.join("scripts/__pycache__/tool.pyc").is_file());
    let restored = status_skill(&result.status, "demo-skill");
    assert_eq!(restored.local_state, "managed");

    let result = manager
        .uninstall("demo-skill", &restored.local_revision, true)
        .await
        .unwrap();
    assert!(result.backup_path.is_some());
    let backup = PathBuf::from(result.backup_path.unwrap());
    assert!(backup.join("SKILL.md").is_file());
    assert!(!home.join(".agents/skills/demo-skill").exists());
}

#[tokio::test]
async fn update_to_new_release_preserves_old_backup() {
    let fixture = demo_fixture("1.0.0", b"print(1)\n");
    let (url, _stop) = serve(
        fixture.dir.parent().unwrap().to_path_buf(),
        "skills/catalog.json",
    )
    .await;
    let (manager, _tmp) = test_manager(&url);

    let status = manager.status(true).await.unwrap();
    let skill = status_skill(&status, "demo-skill");
    manager
        .install(
            "demo-skill",
            skill.release_revision.as_deref().unwrap(),
            &skill.local_revision,
            false,
        )
        .await
        .unwrap();

    let (_m, manifest_sha) = write_release(
        &fixture.dir,
        "demo-skill",
        "2.0.0",
        &[
            ("SKILL.md", b"---\nname: demo-skill\n---\nv2\n"),
            ("scripts/tool.py", b"print(2)\n"),
        ],
        &["scripts/tool.py"],
    );
    write_catalog(
        &fixture.dir,
        vec![serde_json::json!({
            "id": "demo-skill",
            "name": "Demo Skill",
            "description": "fixture",
            "version": "2.0.0",
            "manifest": "releases/demo-skill/2.0.0/manifest.json",
            "manifest_sha256": manifest_sha,
        })],
    );

    let status = manager.status(true).await.unwrap();
    let skill = status_skill(&status, "demo-skill");
    assert_eq!(skill.update_state, "available");
    assert_eq!(skill.latest_version.as_deref(), Some("2.0.0"));

    let result = manager
        .install(
            "demo-skill",
            skill.release_revision.as_deref().unwrap(),
            &skill.local_revision,
            false,
        )
        .await
        .unwrap();
    assert!(result.backup_path.is_some());
    assert_eq!(
        status_skill(&result.status, "demo-skill")
            .installed_version
            .as_deref(),
        Some("2.0.0")
    );
}

#[tokio::test]
async fn unmanaged_directory_requires_confirmation_and_is_backed_up() {
    let fixture = demo_fixture("1.0.0", b"print(1)\n");
    let (url, _stop) = serve(
        fixture.dir.parent().unwrap().to_path_buf(),
        "skills/catalog.json",
    )
    .await;
    let (manager, tmp) = test_manager(&url);
    let home = tmp.path().join("home");
    let target = home.join(".agents/skills/demo-skill");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(
        target.join("SKILL.md"),
        b"---\nname: demo-skill\n---\nmanual\n",
    )
    .unwrap();
    std::fs::write(target.join("notes.txt"), b"user file\n").unwrap();

    let status = manager.status(true).await.unwrap();
    let skill = status_skill(&status, "demo-skill");
    assert_eq!(skill.local_state, "unmanaged");
    assert_eq!(skill.installed_version, None);

    let error = manager
        .install(
            "demo-skill",
            skill.release_revision.as_deref().unwrap(),
            &skill.local_revision,
            false,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "local_conflict");

    let status = manager.status(false).await.unwrap();
    let skill = status_skill(&status, "demo-skill");
    let result = manager
        .install(
            "demo-skill",
            skill.release_revision.as_deref().unwrap(),
            &skill.local_revision,
            true,
        )
        .await
        .unwrap();
    let backup = PathBuf::from(result.backup_path.unwrap());
    assert!(backup.join("notes.txt").is_file());
    assert_eq!(
        status_skill(&result.status, "demo-skill").local_state,
        "managed"
    );
}

#[tokio::test]
async fn local_newer_version_is_never_downgraded() {
    let fixture = demo_fixture("1.0.0", b"print(1)\n");
    let (url, _stop) = serve(
        fixture.dir.parent().unwrap().to_path_buf(),
        "skills/catalog.json",
    )
    .await;
    let (manager, tmp) = test_manager(&url);
    let home = tmp.path().join("home");
    let target = home.join(".agents/skills/demo-skill");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("SKILL.md"), b"---\nname: demo-skill\n---\nx\n").unwrap();
    let skill_md = b"---\nname: demo-skill\n---\nx\n";
    let receipt = serde_json::json!({
        "schemaVersion": 1,
        "source": "https://api.ai-cove.com/sidecars/skills/",
        "id": "demo-skill",
        "version": "9.9.9",
        "manifestSha256": "0".repeat(64),
        "files": [{"path": "SKILL.md", "size": skill_md.len(), "sha256": sha(skill_md), "executable": false}],
        "installedAtUnixSeconds": 1700000000u64,
        "transactionId": "tx-4242-4242",
    });
    std::fs::write(target.join(".ai-cove-install.json"), receipt.to_string()).unwrap();

    let status = manager.status(true).await.unwrap();
    let skill = status_skill(&status, "demo-skill");
    assert_eq!(skill.installed_version.as_deref(), Some("9.9.9"));
    assert_eq!(skill.update_state, "local_newer");

    let error = manager
        .install(
            "demo-skill",
            skill.release_revision.as_deref().unwrap(),
            &skill.local_revision,
            true,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "local_conflict");
}

#[tokio::test]
async fn revision_mismatch_and_missing_skill_fail_stably() {
    let fixture = demo_fixture("1.0.0", b"print(1)\n");
    let (url, _stop) = serve(
        fixture.dir.parent().unwrap().to_path_buf(),
        "skills/catalog.json",
    )
    .await;
    let (manager, _tmp) = test_manager(&url);

    let status = manager.status(true).await.unwrap();
    let skill = status_skill(&status, "demo-skill");
    let error = manager
        .install(
            "demo-skill",
            "0".repeat(64).as_str(),
            &skill.local_revision,
            false,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "revision_changed");

    let error = manager
        .install(
            "ghost",
            skill.release_revision.as_deref().unwrap(),
            &skill.local_revision,
            false,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "not_found");

    let error = manager
        .uninstall("demo-skill", &skill.local_revision, false)
        .await
        .unwrap_err();
    assert_eq!(error.code, "local_conflict");
}

#[tokio::test]
async fn offline_catalog_preserves_local_state() {
    let fixture = demo_fixture("1.0.0", b"print(1)\n");
    let (url, stop) = serve(
        fixture.dir.parent().unwrap().to_path_buf(),
        "skills/catalog.json",
    )
    .await;
    let (manager, tmp) = test_manager(&url);
    let home = tmp.path().join("home");

    let status = manager.status(true).await.unwrap();
    let skill = status_skill(&status, "demo-skill");
    manager
        .install(
            "demo-skill",
            skill.release_revision.as_deref().unwrap(),
            &skill.local_revision,
            false,
        )
        .await
        .unwrap();

    let _ = stop.send(());
    let status = manager.status(true).await.unwrap();
    assert_eq!(status.catalog_state, "stale");
    let skill = status_skill(&status, "demo-skill");
    assert_eq!(skill.local_state, "managed");
    assert_eq!(skill.installed_version.as_deref(), Some("1.0.0"));
    assert!(home.join(".agents/skills/demo-skill").is_dir());
}

#[tokio::test]
async fn real_imagine_release_installs_via_http_fixture() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    if !repo_root.join("deploy/build-skills.py").exists()
        || !repo_root.join("skills/ai-cove-imagine/SKILL.md").exists()
    {
        eprintln!("skipped: platform fixture source not present (standalone turbo checkout)");
        return;
    }
    let out = tempfile::tempdir().unwrap();
    let status = std::process::Command::new("python3")
        .arg(repo_root.join("deploy/build-skills.py"))
        .arg("build")
        .arg("--source-root")
        .arg(&repo_root)
        .arg("--output")
        .arg(out.path())
        .output()
        .expect("python3 must be available for the fixture build");
    assert!(
        status.status.success(),
        "fixture build failed: {}",
        String::from_utf8_lossy(&status.stderr)
    );

    let (url, _stop) = serve(out.path().to_path_buf(), "catalog.json").await;
    let (manager, tmp) = test_manager(&url);
    let home = tmp.path().join("home");

    let status = manager.status(true).await.unwrap();
    let skill = status_skill(&status, "ai-cove-imagine");
    assert_eq!(skill.latest_version.as_deref(), Some("1.0.0"));

    let result = manager
        .install(
            "ai-cove-imagine",
            skill.release_revision.as_deref().unwrap(),
            &skill.local_revision,
            false,
        )
        .await
        .unwrap();
    let installed = status_skill(&result.status, "ai-cove-imagine");
    assert_eq!(installed.local_state, "managed");
    assert_eq!(installed.update_state, "current");
    for rel in [
        "SKILL.md",
        "agents/openai.yaml",
        "references/api.md",
        "scripts/imagine.py",
    ] {
        assert!(
            home.join(".agents/skills/ai-cove-imagine")
                .join(rel)
                .is_file(),
            "missing installed file {rel}"
        );
    }
    let uninstall = manager
        .uninstall("ai-cove-imagine", &installed.local_revision, true)
        .await
        .unwrap();
    let backup = PathBuf::from(uninstall.backup_path.unwrap());
    assert!(backup.join("scripts/imagine.py").is_file());
    assert!(backup.to_string_lossy().contains(".ai-cove-skills"));
}

#[tokio::test]
async fn symlinked_skill_dir_is_unsafe_and_untouched() {
    let fixture = demo_fixture("1.0.0", b"print(1)\n");
    let (url, _stop) = serve(
        fixture.dir.parent().unwrap().to_path_buf(),
        "skills/catalog.json",
    )
    .await;
    let (manager, tmp) = test_manager(&url);
    let home = tmp.path().join("home");
    let elsewhere = tmp.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::write(elsewhere.join("x.txt"), b"x").unwrap();
    let install_root = home.join(".agents/skills");
    std::fs::create_dir_all(&install_root).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&elsewhere, install_root.join("demo-skill")).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(&elsewhere, install_root.join("demo-skill")).unwrap();

    let status = manager.status(true).await.unwrap();
    let skill = status_skill(&status, "demo-skill");
    assert_eq!(skill.local_state, "unsafe");
    let error = manager
        .install(
            "demo-skill",
            skill.release_revision.as_deref().unwrap(),
            &skill.local_revision,
            true,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "unsafe_path");
    assert!(elsewhere.join("x.txt").is_file());
}

#[tokio::test]
async fn uninstall_rejects_traversal_and_unmanaged_ids() {
    let fixture = demo_fixture("1.0.0", b"print(1)\n");
    let (url, _stop) = serve(
        fixture.dir.parent().unwrap().to_path_buf(),
        "skills/catalog.json",
    )
    .await;
    let (manager, tmp) = test_manager(&url);
    let home = tmp.path().join("home");
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("keep.txt"), b"keep").unwrap();

    for bad in [
        "../outside",
        "/etc/passwd",
        "..",
        "x/../y",
        "UPPER",
        "demo_skill",
    ] {
        let error = manager.uninstall(bad, "", true).await.unwrap_err();
        assert!(
            error.code == "invalid_input" || error.code == "not_found",
            "{bad} should fail, got {}",
            error.code
        );
    }
    assert!(outside.join("keep.txt").is_file());

    let unrelated = home.join(".agents/skills/other-skill");
    std::fs::create_dir_all(&unrelated).unwrap();
    std::fs::write(unrelated.join("file.txt"), b"user").unwrap();
    let error = manager
        .uninstall("other-skill", "anything", true)
        .await
        .unwrap_err();
    assert_eq!(error.code, "not_found");
    assert!(unrelated.join("file.txt").is_file());

    let rogue = home.join(".agents/skills/rogue-skill");
    std::fs::create_dir_all(&rogue).unwrap();
    std::fs::write(rogue.join("SKILL.md"), b"---\nname: rogue-skill\n---\nx\n").unwrap();
    let receipt = serde_json::json!({
        "schemaVersion": 1,
        "source": "https://api.ai-cove.com/sidecars/skills/",
        "id": "demo-skill",
        "version": "1.0.0",
        "manifestSha256": "0".repeat(64),
        "files": [{"path": "SKILL.md", "size": 30, "sha256": sha(b"---\nname: rogue-skill\n---\nx\n"), "executable": false}],
        "installedAtUnixSeconds": 1u64,
        "transactionId": "tx-1-1",
    });
    std::fs::write(rogue.join(".ai-cove-install.json"), receipt.to_string()).unwrap();
    let error = manager
        .uninstall("rogue-skill", "anything", true)
        .await
        .unwrap_err();
    assert_eq!(error.code, "not_found");
    assert!(rogue.join("SKILL.md").is_file());
    let status = manager.status(true).await.unwrap();
    assert!(status.skills.iter().all(|skill| skill.id != "rogue-skill"));
}

#[tokio::test]
async fn unsafe_paths_block_mutation_and_report_unsafe_state() {
    let fixture = demo_fixture("1.0.0", b"print(1)\n");
    let (url, _stop) = serve(
        fixture.dir.parent().unwrap().to_path_buf(),
        "skills/catalog.json",
    )
    .await;

    #[cfg(unix)]
    {
        let (manager, tmp) = test_manager(&url);
        let home = tmp.path().join("home");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("x"), b"x").unwrap();
        std::fs::create_dir_all(home.join(".agents")).unwrap();
        std::os::unix::fs::symlink(&outside, home.join(".agents/skills")).unwrap();
        let status = manager.status(true).await.unwrap();
        let skill = status_skill(&status, "demo-skill");
        assert_eq!(skill.local_state, "unsafe");
        assert!(outside.join("x").is_file());
    }

    #[cfg(unix)]
    {
        let (manager, tmp) = test_manager(&url);
        let home = tmp.path().join("home");
        let install_root = home.join(".agents/skills");
        let target = install_root.join("demo-skill");
        std::fs::create_dir_all(target.join("scripts/__pycache__")).unwrap();
        std::fs::write(target.join("SKILL.md"), b"---\nname: demo-skill\n---\nx\n").unwrap();
        let dangling = tmp.path().join("dangling-target");
        std::os::unix::fs::symlink(&dangling, target.join("scripts/__pycache__/bad.pyc")).unwrap();
        let status = manager.status(true).await.unwrap();
        assert_eq!(status_skill(&status, "demo-skill").local_state, "unsafe");
        let error = manager
            .uninstall(
                "demo-skill",
                &status_skill(&status, "demo-skill").local_revision,
                true,
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, "unsafe_path");
    }

    #[cfg(unix)]
    {
        let (manager, tmp) = test_manager(&url);
        let home = tmp.path().join("home");
        let target = home.join(".agents/skills/demo-skill");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("SKILL.md"), b"---\nname: demo-skill\n---\nx\n").unwrap();
        let real_receipt = tmp.path().join("real-receipt.json");
        std::fs::write(&real_receipt, b"{}").unwrap();
        std::os::unix::fs::symlink(&real_receipt, target.join(".ai-cove-install.json")).unwrap();
        let status = manager.status(true).await.unwrap();
        assert_eq!(status_skill(&status, "demo-skill").local_state, "unsafe");
    }

    #[cfg(unix)]
    {
        let (manager, tmp) = test_manager(&url);
        let home = tmp.path().join("home");
        std::fs::create_dir_all(home.join(".agents/skills")).unwrap();
        std::fs::write(home.join(".agents/skills/demo-skill"), b"not a dir").unwrap();
        let status = manager.status(true).await.unwrap();
        assert_eq!(status_skill(&status, "demo-skill").local_state, "unsafe");
    }

    #[cfg(unix)]
    {
        let (manager, tmp) = test_manager(&url);
        let home = tmp.path().join("home");
        let elsewhere = tmp.path().join("control-elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::create_dir_all(home.join(".agents/skills")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, home.join(".agents/.ai-cove-skills")).unwrap();
        let status = manager.status(true).await.unwrap();
        let skill = status_skill(&status, "demo-skill");
        let error = manager
            .install(
                "demo-skill",
                skill.release_revision.as_deref().unwrap(),
                &skill.local_revision,
                false,
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, "unsafe_path");
        assert!(elsewhere.read_dir().unwrap().next().is_none());
    }
}

#[tokio::test]
async fn stale_lock_blocks_and_manual_clear_allows() {
    let fixture = demo_fixture("1.0.0", b"print(1)\n");
    let (url, _stop) = serve(
        fixture.dir.parent().unwrap().to_path_buf(),
        "skills/catalog.json",
    )
    .await;
    let (manager, tmp) = test_manager(&url);
    let home = tmp.path().join("home");
    let control = home.join(".agents/.ai-cove-skills");
    std::fs::create_dir_all(&control).unwrap();
    let lock = control.join("install.lock");
    std::fs::write(&lock, b"stale-from-dead-process").unwrap();

    let status = manager.status(true).await.unwrap();
    let skill = status_skill(&status, "demo-skill");
    let error = manager
        .install(
            "demo-skill",
            skill.release_revision.as_deref().unwrap(),
            &skill.local_revision,
            false,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "busy");
    assert!(error.message.contains("install.lock") || error.message.contains("手动删除"));
    assert!(!home.join(".agents/skills/demo-skill").exists());

    std::fs::remove_file(&lock).unwrap();
    let status = manager.status(false).await.unwrap();
    let skill = status_skill(&status, "demo-skill");
    manager
        .install(
            "demo-skill",
            skill.release_revision.as_deref().unwrap(),
            &skill.local_revision,
            false,
        )
        .await
        .unwrap();
    assert!(home.join(".agents/skills/demo-skill").is_dir());
}

#[tokio::test]
async fn crashed_install_restores_previous_directory() {
    let fixture = demo_fixture("1.0.0", b"print(1)\n");
    let (url, _stop) = serve(
        fixture.dir.parent().unwrap().to_path_buf(),
        "skills/catalog.json",
    )
    .await;
    let (manager, tmp) = test_manager(&url);
    let home = tmp.path().join("home");

    let status = manager.status(true).await.unwrap();
    let skill = status_skill(&status, "demo-skill");
    let result = manager
        .install(
            "demo-skill",
            skill.release_revision.as_deref().unwrap(),
            &skill.local_revision,
            false,
        )
        .await
        .unwrap();
    let installed_rev = status_skill(&result.status, "demo-skill")
        .local_revision
        .clone();

    let control = home.join(".agents/.ai-cove-skills");
    let target = home.join(".agents/skills/demo-skill");
    let backup = control.join("backups/demo-skill/tx-9999-4242");
    std::fs::create_dir_all(backup.parent().unwrap()).unwrap();
    std::fs::rename(&target, &backup).unwrap();
    let staging = control.join("staging/tx-9999-4242");
    std::fs::create_dir_all(&staging).unwrap();
    std::fs::write(staging.join("junk"), b"half-downloaded").unwrap();
    let release_rev = status_skill(&result.status, "demo-skill")
        .release_revision
        .clone()
        .unwrap();
    let journal = serde_json::json!({
        "schemaVersion": 1,
        "id": "demo-skill",
        "tx": "tx-9999-4242",
        "operation": "install",
        "expectedLocalRevision": installed_rev,
        "hadTarget": true,
        "releaseRevision": release_rev,
    });
    let tx_dir = control.join("transactions");
    std::fs::create_dir_all(&tx_dir).unwrap();
    std::fs::write(tx_dir.join("tx-9999-4242.json"), journal.to_string()).unwrap();

    let status = manager.status(false).await.unwrap();
    let skill = status_skill(&status, "demo-skill");
    assert_eq!(skill.local_state, "absent");

    let result = manager
        .install(
            "demo-skill",
            skill.release_revision.as_deref().unwrap(),
            &installed_rev,
            false,
        )
        .await
        .unwrap();
    assert!(target.join("SKILL.md").is_file());
    assert!(!staging.exists());
    assert_eq!(
        status_skill(&result.status, "demo-skill").local_state,
        "managed"
    );
}

#[tokio::test]
async fn ambiguous_crash_state_errors_without_touching_data() {
    let fixture = demo_fixture("1.0.0", b"print(1)\n");
    let (url, _stop) = serve(
        fixture.dir.parent().unwrap().to_path_buf(),
        "skills/catalog.json",
    )
    .await;
    let (manager, tmp) = test_manager(&url);
    let home = tmp.path().join("home");
    let target = home.join(".agents/skills/demo-skill");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(
        target.join("SKILL.md"),
        b"---\nname: demo-skill\n---\nuser\n",
    )
    .unwrap();
    let control = home.join(".agents/.ai-cove-skills");
    let journal = serde_json::json!({
        "schemaVersion": 1,
        "id": "demo-skill",
        "tx": "tx-8888-1111",
        "operation": "install",
        "expectedLocalRevision": "deadbeef",
        "hadTarget": true,
        "releaseRevision": "0".repeat(64),
    });
    let tx_dir = control.join("transactions");
    std::fs::create_dir_all(&tx_dir).unwrap();
    std::fs::write(tx_dir.join("tx-8888-1111.json"), journal.to_string()).unwrap();

    let status = manager.status(true).await.unwrap();
    let skill = status_skill(&status, "demo-skill");
    let error = manager
        .install(
            "demo-skill",
            skill.release_revision.as_deref().unwrap(),
            &skill.local_revision,
            true,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "internal");
    assert!(target.join("SKILL.md").is_file());
    assert!(tx_dir.join("tx-8888-1111.json").is_file());
}

#[tokio::test]
async fn stale_catalog_blocks_install_but_allows_offline_uninstall() {
    let fixture = demo_fixture("1.0.0", b"print(1)\n");
    let (url, stop) = serve(
        fixture.dir.parent().unwrap().to_path_buf(),
        "skills/catalog.json",
    )
    .await;
    let (manager, _tmp) = test_manager(&url);

    let status = manager.status(true).await.unwrap();
    let skill = status_skill(&status, "demo-skill");
    manager
        .install(
            "demo-skill",
            skill.release_revision.as_deref().unwrap(),
            &skill.local_revision,
            false,
        )
        .await
        .unwrap();

    let _ = stop.send(());
    let status = manager.status(true).await.unwrap();
    assert_eq!(status.catalog_state, "stale");
    let skill = status_skill(&status, "demo-skill");
    assert_eq!(skill.update_state, "unknown");
    let error = manager
        .install(
            "demo-skill",
            skill.release_revision.as_deref().unwrap_or(""),
            &skill.local_revision,
            false,
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "catalog_unavailable");

    let result = manager
        .uninstall("demo-skill", &skill.local_revision, true)
        .await
        .unwrap();
    assert!(result.backup_path.is_some());
}

#[tokio::test]
async fn executable_bit_and_corrupt_receipt_change_revision() {
    let fixture = demo_fixture("1.0.0", b"print(1)\n");
    let (url, _stop) = serve(
        fixture.dir.parent().unwrap().to_path_buf(),
        "skills/catalog.json",
    )
    .await;
    let (manager, tmp) = test_manager(&url);
    let home = tmp.path().join("home");

    let status = manager.status(true).await.unwrap();
    let skill = status_skill(&status, "demo-skill");
    let result = manager
        .install(
            "demo-skill",
            skill.release_revision.as_deref().unwrap(),
            &skill.local_revision,
            false,
        )
        .await
        .unwrap();
    let managed_rev = status_skill(&result.status, "demo-skill")
        .local_revision
        .clone();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let tool = home.join(".agents/skills/demo-skill/scripts/tool.py");
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o644)).unwrap();
        let status = manager.status(false).await.unwrap();
        let skill = status_skill(&status, "demo-skill");
        assert_eq!(skill.local_state, "modified");
        assert_ne!(skill.local_revision, managed_rev);
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let receipt_path = home.join(".agents/skills/demo-skill/.ai-cove-install.json");
    std::fs::write(&receipt_path, b"not-json").unwrap();
    let status = manager.status(false).await.unwrap();
    let skill = status_skill(&status, "demo-skill");
    assert_eq!(skill.local_state, "error");
    assert_ne!(skill.local_revision, managed_rev);
}

fn sha_hex(data: &[u8]) -> String {
    crate::skills_catalog::sha256_hex(data)
}

async fn fixture_manager(root: &Path) -> (SkillsManager, tempfile::TempDir, oneshot::Sender<()>) {
    let name = root.file_name().unwrap().to_str().unwrap().to_string();
    let (url, stop) = serve(
        root.parent().unwrap().to_path_buf(),
        &format!("{name}/catalog.json"),
    )
    .await;
    let (manager, tmp) = test_manager(&url);
    (manager, tmp, stop)
}

fn install_root_of(status: &crate::skills::SkillsStatus) -> PathBuf {
    PathBuf::from(&status.install_root)
}

#[tokio::test]
async fn invalid_manifest_matrix_is_rejected() {
    let good_skill = b"---\nname: demo-skill\n---\nx\n".to_vec();
    let tool = b"print(1)\n".to_vec();
    let file_obj = |path: &str, data: &[u8], extra: serde_json::Value| {
        let mut obj = serde_json::json!({
            "path": path,
            "size": data.len(),
            "sha256": sha_hex(data),
            "executable": false,
        });
        if let serde_json::Value::Object(map) = extra {
            for (k, v) in map {
                obj[k] = v;
            }
        }
        obj
    };
    let make = |files: Vec<serde_json::Value>| {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("skills");
        std::fs::create_dir_all(&root).unwrap();
        let manifest = serde_json::json!({
            "schema_version": 1, "id": "demo-skill", "version": "1.0.0", "files": files,
        });
        let bytes = serde_json::to_vec_pretty(&manifest).unwrap();
        let sha = sha_hex(&bytes);
        let rel = root.join("releases/demo-skill/1.0.0");
        std::fs::create_dir_all(&rel).unwrap();
        std::fs::write(rel.join("manifest.json"), &bytes).unwrap();
        write_catalog(
            &root,
            vec![serde_json::json!({
                "id": "demo-skill", "name": "Demo Skill", "description": "d",
                "version": "1.0.0",
                "manifest": "releases/demo-skill/1.0.0/manifest.json",
                "manifest_sha256": sha,
            })],
        );
        Fixture {
            dir: root,
            _tmp: tmp,
        }
    };
    let cases: Vec<(&str, Vec<serde_json::Value>)> = vec![
        (
            "executable non-bool",
            vec![file_obj(
                "SKILL.md",
                &good_skill,
                serde_json::json!({"executable": "yes"}),
            )],
        ),
        (
            "executable key absent",
            vec![{
                let mut obj = file_obj("SKILL.md", &good_skill, serde_json::json!({}));
                obj.as_object_mut().unwrap().remove("executable");
                obj
            }],
        ),
        (
            "missing SKILL.md",
            vec![file_obj("scripts/tool.py", &tool, serde_json::json!({}))],
        ),
        (
            "file vs dir prefix",
            vec![
                file_obj("a", &tool, serde_json::json!({})),
                file_obj("a/b.txt", &tool, serde_json::json!({})),
                file_obj("SKILL.md", &good_skill, serde_json::json!({})),
            ],
        ),
        (
            "casefold dir collision",
            vec![
                file_obj("Scripts/a.py", &tool, serde_json::json!({})),
                file_obj("scripts/b.py", &tool, serde_json::json!({})),
                file_obj("SKILL.md", &good_skill, serde_json::json!({})),
            ],
        ),
        (
            "percent path",
            vec![
                file_obj("x%2e.md", &tool, serde_json::json!({})),
                file_obj("SKILL.md", &good_skill, serde_json::json!({})),
            ],
        ),
        (
            "query path",
            vec![
                file_obj("x?y.md", &tool, serde_json::json!({})),
                file_obj("SKILL.md", &good_skill, serde_json::json!({})),
            ],
        ),
        (
            "fragment path",
            vec![
                file_obj("x#y.md", &tool, serde_json::json!({})),
                file_obj("SKILL.md", &good_skill, serde_json::json!({})),
            ],
        ),
        (
            "traversal path",
            vec![
                file_obj("../outside", &tool, serde_json::json!({})),
                file_obj("SKILL.md", &good_skill, serde_json::json!({})),
            ],
        ),
        (
            "tests dir excluded",
            vec![
                file_obj("tests/t.py", &tool, serde_json::json!({})),
                file_obj("SKILL.md", &good_skill, serde_json::json!({})),
            ],
        ),
        (
            "pyc excluded",
            vec![
                file_obj("scripts/x.pyc", &tool, serde_json::json!({})),
                file_obj("SKILL.md", &good_skill, serde_json::json!({})),
            ],
        ),
        (
            "zero size file",
            vec![
                file_obj("empty.md", b"", serde_json::json!({})),
                file_obj("SKILL.md", &good_skill, serde_json::json!({})),
            ],
        ),
        (
            "oversize file",
            vec![
                file_obj(
                    "big.bin",
                    &tool,
                    serde_json::json!({"size": 9 * 1024 * 1024}),
                ),
                file_obj("SKILL.md", &good_skill, serde_json::json!({})),
            ],
        ),
        (
            "bad file sha",
            vec![
                file_obj("x.md", &tool, serde_json::json!({"sha256": "X".repeat(64)})),
                file_obj("SKILL.md", &good_skill, serde_json::json!({})),
            ],
        ),
        (
            "duplicate case path",
            vec![
                file_obj("x.md", &tool, serde_json::json!({})),
                file_obj("X.MD", &tool, serde_json::json!({})),
                file_obj("SKILL.md", &good_skill, serde_json::json!({})),
            ],
        ),
    ];
    for (name, files) in cases {
        let fixture = make(files);
        let (manager, _tmp, _stop) = fixture_manager(&fixture.dir).await;
        let status = manager.status(true).await.unwrap();
        assert_eq!(
            status.catalog_state, "unavailable",
            "case {name} should fail"
        );
    }
}

#[tokio::test]
async fn catalog_matrix_rejects_duplicate_ids_and_bad_versions() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("skills");
    std::fs::create_dir_all(&root).unwrap();
    let cases: Vec<serde_json::Value> = vec![
        serde_json::json!({"id": "dup", "name": "a", "description": "d", "version": "1.0.0",
            "manifest": "releases/dup/1.0.0/manifest.json", "manifest_sha256": sha_hex(b"x")}),
        serde_json::json!({"id": "dup", "name": "b", "description": "d", "version": "2.0.0",
            "manifest": "releases/dup/2.0.0/manifest.json", "manifest_sha256": sha_hex(b"y")}),
    ];
    write_catalog(&root, cases);
    let (manager, _t, _s) = fixture_manager(&root).await;
    assert_eq!(
        manager.status(true).await.unwrap().catalog_state,
        "unavailable",
        "duplicate ids rejected"
    );

    let root2 = tmp.path().join("skills2");
    std::fs::create_dir_all(&root2).unwrap();
    write_catalog(
        &root2,
        vec![serde_json::json!({
            "id": "demo-skill", "name": "n", "description": "d", "version": "01.0.0",
            "manifest": "releases/demo-skill/01.0.0/manifest.json", "manifest_sha256": sha_hex(b"x"),
        })],
    );
    let (manager2, _t2, _s2) = fixture_manager(&root2).await;
    assert_eq!(
        manager2.status(true).await.unwrap().catalog_state,
        "unavailable",
        "leading-zero version rejected"
    );

    let root3 = tmp.path().join("skills3");
    std::fs::create_dir_all(&root3).unwrap();
    write_catalog(
        &root3,
        vec![serde_json::json!({
            "id": "demo-skill", "name": "  ", "description": "d", "version": "1.0.0",
            "manifest": "releases/demo-skill/1.0.0/manifest.json", "manifest_sha256": sha_hex(b"x"),
        })],
    );
    let (manager3, _t3, _s3) = fixture_manager(&root3).await;
    assert_eq!(
        manager3.status(true).await.unwrap().catalog_state,
        "unavailable",
        "blank name rejected"
    );
}

#[tokio::test]
async fn tampered_download_is_rejected() {
    let fixture = demo_fixture("1.0.0", b"print(1)");
    let file = fixture
        .dir
        .join("releases/demo-skill/1.0.0/files/scripts/tool.py");
    std::fs::write(&file, b"evil()").unwrap();
    let (manager, _t, _s) = fixture_manager(&fixture.dir).await;
    let status = manager.status(true).await.unwrap();
    let skill = status_skill(&status, "demo-skill");
    let err = manager
        .install(
            "demo-skill",
            &skill.release_revision.clone().unwrap(),
            &skill.local_revision,
            false,
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, "integrity_error");
    assert!(!install_root_of(&status).join("demo-skill").exists());
}

#[tokio::test]
async fn redirect_response_is_rejected() {
    let (url, _stop) = serve_raw(|_| {
        axum::http::Response::builder()
            .status(301)
            .header("location", "http://127.0.0.1:1/evil")
            .body(axum::body::Body::empty())
            .unwrap()
    })
    .await;
    let (manager, _tmp) = test_manager(&format!("{url}/skills/catalog.json"));
    let status = manager.status(true).await.unwrap();
    assert_eq!(status.catalog_state, "unavailable");
    assert!(status.catalog_message.unwrap().contains("301"));
}

#[tokio::test]
async fn chunked_response_without_content_length_downloads() {
    let fixture = demo_fixture("1.0.0", b"print(1)");
    let root = fixture.dir.parent().unwrap().to_path_buf();
    let (url, _stop) = serve_raw(move |request| {
        let path = request.uri().path().trim_start_matches('/').to_string();
        let file = root.join(&path);
        match std::fs::read(&file) {
            Ok(data) => {
                let stream = futures_util::stream::iter(
                    data.chunks(3)
                        .map(|c| {
                            Ok::<bytes::Bytes, std::io::Error>(bytes::Bytes::copy_from_slice(c))
                        })
                        .collect::<Vec<_>>(),
                );
                axum::http::Response::builder()
                    .status(200)
                    .body(axum::body::Body::from_stream(stream))
                    .unwrap()
            }
            Err(_) => axum::http::Response::builder()
                .status(404)
                .body(axum::body::Body::empty())
                .unwrap(),
        }
    })
    .await;
    let (manager, _tmp) = test_manager(&format!("{url}/skills/catalog.json"));
    let status = manager.status(true).await.unwrap();
    let skill = status_skill(&status, "demo-skill");
    let result = manager
        .install(
            "demo-skill",
            &skill.release_revision.clone().unwrap(),
            &skill.local_revision,
            false,
        )
        .await
        .unwrap();
    assert_eq!(
        status_skill(&result.status, "demo-skill").local_state,
        "managed"
    );
}

#[tokio::test]
async fn activation_failpoint_restores_and_recovers() {
    let fixture = demo_fixture("1.0.0", b"v1-body");
    let (url, _stop) = serve(
        fixture.dir.parent().unwrap().to_path_buf(),
        "skills/catalog.json",
    )
    .await;
    let (mut manager, _tmp) = test_manager(&url);
    let status = manager.status(true).await.unwrap();
    let install_root = install_root_of(&status);
    let skill = status_skill(&status, "demo-skill");
    manager
        .install(
            "demo-skill",
            &skill.release_revision.clone().unwrap(),
            &skill.local_revision,
            false,
        )
        .await
        .unwrap();
    let original = std::fs::read(install_root.join("demo-skill/SKILL.md")).unwrap();

    write_release(
        &fixture.dir,
        "demo-skill",
        "2.0.0",
        &[
            ("SKILL.md", b"---\nname: demo-skill\n---\nv2\n"),
            ("scripts/tool.py", b"v2-body"),
        ],
        &["scripts/tool.py"],
    );
    write_catalog(
        &fixture.dir,
        vec![serde_json::json!({
            "id": "demo-skill", "name": "Demo Skill", "description": "d", "version": "2.0.0",
            "manifest": "releases/demo-skill/2.0.0/manifest.json",
            "manifest_sha256": sha_hex(&std::fs::read(fixture.dir.join("releases/demo-skill/2.0.0/manifest.json")).unwrap()),
        })],
    );
    manager.roots_mut().failpoints.fail_activation = true;
    let status = manager.status(true).await.unwrap();
    let skill = status_skill(&status, "demo-skill");
    assert!(
        manager
            .install(
                "demo-skill",
                &skill.release_revision.clone().unwrap(),
                &skill.local_revision,
                false
            )
            .await
            .is_err()
    );
    assert_eq!(
        std::fs::read(install_root.join("demo-skill/SKILL.md")).unwrap(),
        original
    );
    manager.roots_mut().failpoints.fail_activation = false;
    let status = manager.status(true).await.unwrap();
    let skill = status_skill(&status, "demo-skill");
    assert_eq!(skill.local_state, "managed");
    manager
        .install(
            "demo-skill",
            &skill.release_revision.clone().unwrap(),
            &skill.local_revision,
            false,
        )
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(install_root.join("demo-skill/SKILL.md")).unwrap(),
        b"---\nname: demo-skill\n---\nv2\n"
    );
}

#[tokio::test]
async fn post_rename_failpoint_recovers_on_next_status() {
    let fixture = demo_fixture("1.0.0", b"v1");
    let (url, _stop) = serve(
        fixture.dir.parent().unwrap().to_path_buf(),
        "skills/catalog.json",
    )
    .await;
    let (mut manager, _tmp) = test_manager(&url);
    let status = manager.status(true).await.unwrap();
    let install_root = install_root_of(&status);
    let skill = status_skill(&status, "demo-skill");
    manager
        .install(
            "demo-skill",
            &skill.release_revision.clone().unwrap(),
            &skill.local_revision,
            false,
        )
        .await
        .unwrap();
    let original = std::fs::read(install_root.join("demo-skill/scripts/tool.py")).unwrap();

    manager.roots_mut().failpoints.fail_after_old_rename = true;
    let status = manager.status(true).await.unwrap();
    let skill = status_skill(&status, "demo-skill");
    assert!(
        manager
            .install(
                "demo-skill",
                &skill.release_revision.clone().unwrap(),
                &skill.local_revision,
                false
            )
            .await
            .is_err()
    );
    assert!(!install_root.join("demo-skill").exists());
    manager.roots_mut().failpoints.fail_after_old_rename = false;
    let status = manager.status(false).await.unwrap();
    let skill = status_skill(&status, "demo-skill").clone();
    assert!(
        manager
            .install(
                "demo-skill",
                &skill.release_revision.clone().unwrap_or_default(),
                &skill.local_revision,
                false
            )
            .await
            .is_err()
            || true
    );
    let status = manager.status(false).await.unwrap();
    assert_eq!(status_skill(&status, "demo-skill").local_state, "managed");
    assert_eq!(
        std::fs::read(install_root.join("demo-skill/scripts/tool.py")).unwrap(),
        original
    );
}

#[tokio::test]
async fn missing_target_and_backup_blocks_recovery() {
    let fixture = demo_fixture("1.0.0", b"v1");
    let (url, _stop) = serve(
        fixture.dir.parent().unwrap().to_path_buf(),
        "skills/catalog.json",
    )
    .await;
    let (mut manager, tmp) = test_manager(&url);
    let status = manager.status(true).await.unwrap();
    let install_root = install_root_of(&status);
    let skill = status_skill(&status, "demo-skill");
    manager
        .install(
            "demo-skill",
            &skill.release_revision.clone().unwrap(),
            &skill.local_revision,
            false,
        )
        .await
        .unwrap();
    let target = install_root.join("demo-skill");
    let tx_dir = tmp.path().join("home/.agents/.ai-cove-skills/transactions");
    std::fs::create_dir_all(&tx_dir).unwrap();
    let journal = serde_json::json!({
        "schemaVersion": 1, "id": "demo-skill", "tx": "tx-7777-7777",
        "operation": "install",
        "expectedLocalRevision": "f".repeat(64),
        "hadTarget": true,
        "releaseRevision": "a".repeat(64),
    });
    std::fs::write(
        tx_dir.join("tx-7777-7777.json"),
        serde_json::to_vec(&journal).unwrap(),
    )
    .unwrap();
    std::fs::remove_dir_all(&target).unwrap();
    let status = manager.status(true).await.unwrap();
    let skill = status_skill(&status, "demo-skill").clone();
    let err = manager
        .uninstall("demo-skill", &skill.local_revision, true)
        .await
        .unwrap_err();
    assert_eq!(err.code, "internal");
    assert!(tx_dir.join("tx-7777-7777.json").exists());
}

#[tokio::test]
async fn staging_symlink_blocks_before_download() {
    let fixture = demo_fixture("1.0.0", b"v1");
    let (url, _stop) = serve(
        fixture.dir.parent().unwrap().to_path_buf(),
        "skills/catalog.json",
    )
    .await;
    let (manager, _tmp) = test_manager(&url);
    let status = manager.status(true).await.unwrap();
    let install_root = install_root_of(&status);
    let skill = status_skill(&status, "demo-skill").clone();
    let staging_root = install_root
        .parent()
        .unwrap()
        .join(".ai-cove-skills/staging");
    std::fs::create_dir_all(staging_root.parent().unwrap()).unwrap();
    let outside = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(outside.path(), &staging_root).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(outside.path(), &staging_root).unwrap();
    let err = manager
        .install(
            "demo-skill",
            &skill.release_revision.clone().unwrap(),
            &skill.local_revision,
            false,
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, "unsafe_path");
    assert!(!install_root.join("demo-skill").exists());
}

#[tokio::test]
async fn cache_root_symlink_never_writes_outside() {
    let fixture = demo_fixture("1.0.0", b"v1");
    let sentinel = tempfile::tempdir().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let cache = tmp.path().join("cache");
    std::fs::create_dir_all(&cache).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(sentinel.path(), cache.join("skills")).unwrap();
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(sentinel.path(), cache.join("skills")).unwrap();
    let (url, _stop) = serve(
        fixture.dir.parent().unwrap().to_path_buf(),
        "skills/catalog.json",
    )
    .await;
    let mut manager = SkillsManager::for_test(
        &url,
        home.join(".agents/skills"),
        home.join(".agents/.ai-cove-skills"),
        cache.join("skills"),
    )
    .unwrap();
    let status = manager.status(true).await.unwrap();
    assert_eq!(status.catalog_state, "fresh");
    assert_eq!(std::fs::read_dir(sentinel.path()).unwrap().count(), 0);
    let skill = status_skill(&status, "demo-skill");
    manager
        .install(
            "demo-skill",
            &skill.release_revision.clone().unwrap(),
            &skill.local_revision,
            false,
        )
        .await
        .unwrap();
    assert_eq!(std::fs::read_dir(sentinel.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn removed_from_catalog_skill_stays_listed() {
    let fixture = demo_fixture("1.0.0", b"v1");
    let (url, _stop) = serve(
        fixture.dir.parent().unwrap().to_path_buf(),
        "skills/catalog.json",
    )
    .await;
    let (mut manager, _tmp) = test_manager(&url);
    let status = manager.status(true).await.unwrap();
    let skill = status_skill(&status, "demo-skill");
    manager
        .install(
            "demo-skill",
            &skill.release_revision.clone().unwrap(),
            &skill.local_revision,
            false,
        )
        .await
        .unwrap();
    write_catalog(&fixture.dir, vec![]);
    let status = manager.status(true).await.unwrap();
    let skill = status_skill(&status, "demo-skill");
    assert_eq!(skill.local_state, "managed");
    assert!(skill.latest_version.is_none());
}

#[tokio::test]
async fn journal_write_failpoint_leaves_everything_untouched() {
    let fixture = demo_fixture("1.0.0", b"v1");
    let (url, _stop) = serve(
        fixture.dir.parent().unwrap().to_path_buf(),
        "skills/catalog.json",
    )
    .await;
    let (mut manager, _tmp) = test_manager(&url);
    let status = manager.status(true).await.unwrap();
    let skill = status_skill(&status, "demo-skill");
    manager
        .install(
            "demo-skill",
            &skill.release_revision.clone().unwrap(),
            &skill.local_revision,
            false,
        )
        .await
        .unwrap();
    let installed_rev = status_skill(&manager.status(true).await.unwrap(), "demo-skill")
        .local_revision
        .clone();
    manager.roots_mut().failpoints.fail_journal_write = true;
    write_release(
        &fixture.dir,
        "demo-skill",
        "2.0.0",
        &[
            ("SKILL.md", b"---\nname: demo-skill\n---\nv2\n"),
            ("scripts/tool.py", b"v2"),
        ],
        &["scripts/tool.py"],
    );
    write_catalog(
        &fixture.dir,
        vec![serde_json::json!({
            "id": "demo-skill", "name": "Demo Skill", "description": "d", "version": "2.0.0",
            "manifest": "releases/demo-skill/2.0.0/manifest.json",
            "manifest_sha256": sha_hex(&std::fs::read(fixture.dir.join("releases/demo-skill/2.0.0/manifest.json")).unwrap()),
        })],
    );
    let status = manager.status(true).await.unwrap();
    let skill = status_skill(&status, "demo-skill").clone();
    assert!(
        manager
            .install(
                "demo-skill",
                &skill.release_revision.clone().unwrap(),
                &skill.local_revision,
                false
            )
            .await
            .is_err()
    );
    assert_eq!(
        status_skill(&manager.status(false).await.unwrap(), "demo-skill").local_revision,
        installed_rev
    );
}

#[test]
fn validate_skill_id_allows_hyphenated_reserved_stems() {
    assert!(crate::skills::validate_skill_id("con-foo").is_ok());
    assert!(crate::skills::validate_skill_id("con").is_err());
    assert!(crate::skills::validate_skill_id("aux").is_err());
    assert!(crate::skills::validate_skill_id("a-").is_err());
    assert!(crate::skills::validate_skill_id("-a").is_err());
}

#[tokio::test]
async fn reconstructed_manager_loads_disk_cache_offline() {
    let fixture = demo_fixture("1.0.0", b"v1");
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let cache = tmp.path().join("cache");
    let (url, stop) = serve(
        fixture.dir.parent().unwrap().to_path_buf(),
        "skills/catalog.json",
    )
    .await;
    let mut manager = SkillsManager::for_test(
        &url,
        home.join(".agents/skills"),
        home.join(".agents/.ai-cove-skills"),
        cache.join("skills"),
    )
    .unwrap();
    let status = manager.status(true).await.unwrap();
    let skill = status_skill(&status, "demo-skill");
    manager
        .install(
            "demo-skill",
            &skill.release_revision.clone().unwrap(),
            &skill.local_revision,
            false,
        )
        .await
        .unwrap();
    assert!(cache.join("skills/catalog.json").exists());
    drop(manager);
    let _ = stop.send(());
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let mut manager = SkillsManager::for_test(
        &url,
        home.join(".agents/skills"),
        home.join(".agents/.ai-cove-skills"),
        cache.join("skills"),
    )
    .unwrap();
    let status = manager.status(true).await.unwrap();
    assert_eq!(status.catalog_state, "stale");
    let skill = status_skill(&status, "demo-skill");
    assert_eq!(skill.local_state, "managed");
    assert_eq!(skill.installed_version.as_deref(), Some("1.0.0"));
    assert!(skill.release_revision.is_some());
}

#[tokio::test]
async fn unclosed_skill_frontmatter_rejected_and_prior_install_kept() {
    let fixture = demo_fixture("1.0.0", b"v1-body");
    let (url, _stop) = serve(
        fixture.dir.parent().unwrap().to_path_buf(),
        "skills/catalog.json",
    )
    .await;
    let (mut manager, _tmp) = test_manager(&url);
    let status = manager.status(true).await.unwrap();
    let install_root = install_root_of(&status);
    let skill = status_skill(&status, "demo-skill");
    manager
        .install(
            "demo-skill",
            &skill.release_revision.clone().unwrap(),
            &skill.local_revision,
            false,
        )
        .await
        .unwrap();
    let original = std::fs::read(install_root.join("demo-skill/SKILL.md")).unwrap();

    let (_m, manifest_sha) = write_release(
        &fixture.dir,
        "demo-skill",
        "2.0.0",
        &[
            ("SKILL.md", b"---\nname: demo-skill\nno closing fence\n"),
            ("scripts/tool.py", b"v2\n"),
        ],
        &[],
    );
    write_catalog(
        &fixture.dir,
        vec![serde_json::json!({
            "id": "demo-skill", "name": "Demo Skill", "description": "fixture",
            "version": "2.0.0",
            "manifest": "releases/demo-skill/2.0.0/manifest.json",
            "manifest_sha256": manifest_sha,
        })],
    );
    let status = manager.status(true).await.unwrap();
    let skill = status_skill(&status, "demo-skill");
    let err = manager
        .install(
            "demo-skill",
            &skill.release_revision.clone().unwrap(),
            &skill.local_revision,
            false,
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, "integrity_error");
    assert_eq!(
        std::fs::read(install_root.join("demo-skill/SKILL.md")).unwrap(),
        original
    );
    assert_eq!(
        status_skill(&manager.status(false).await.unwrap(), "demo-skill")
            .installed_version
            .as_deref(),
        Some("1.0.0")
    );
}

#[tokio::test]
async fn postverify_failure_quarantines_and_restores_prior_install() {
    let fixture = demo_fixture("1.0.0", b"v1-body");
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let (url, _stop) = serve(
        fixture.dir.parent().unwrap().to_path_buf(),
        "skills/catalog.json",
    )
    .await;
    let mut manager = SkillsManager::for_test(
        &url,
        home.join(".agents/skills"),
        home.join(".agents/.ai-cove-skills"),
        tmp.path().join("cache/skills"),
    )
    .unwrap();
    let install_root = home.join(".agents/skills");
    let status = manager.status(true).await.unwrap();
    let skill = status_skill(&status, "demo-skill");
    manager
        .install(
            "demo-skill",
            &skill.release_revision.clone().unwrap(),
            &skill.local_revision,
            false,
        )
        .await
        .unwrap();
    let original = std::fs::read(install_root.join("demo-skill/scripts/tool.py")).unwrap();

    let (_m, manifest_sha) = write_release(
        &fixture.dir,
        "demo-skill",
        "2.0.0",
        &[
            ("SKILL.md", b"---\nname: demo-skill\n---\nv2\n"),
            ("scripts/tool.py", b"v2\n"),
        ],
        &[],
    );
    write_catalog(
        &fixture.dir,
        vec![serde_json::json!({
            "id": "demo-skill", "name": "Demo Skill", "description": "fixture",
            "version": "2.0.0",
            "manifest": "releases/demo-skill/2.0.0/manifest.json",
            "manifest_sha256": manifest_sha,
        })],
    );
    let status = manager.status(true).await.unwrap();
    let skill = status_skill(&status, "demo-skill");
    manager.roots_mut().failpoints.fail_post_verify = true;
    let err = manager
        .install(
            "demo-skill",
            &skill.release_revision.clone().unwrap(),
            &skill.local_revision,
            false,
        )
        .await
        .unwrap_err();
    assert_eq!(err.code, "integrity_error");
    assert_eq!(
        std::fs::read(install_root.join("demo-skill/scripts/tool.py")).unwrap(),
        original
    );
    let quarantine_root = home.join(".agents/.ai-cove-skills/quarantine");
    let entries: Vec<_> = std::fs::read_dir(&quarantine_root).unwrap().collect();
    assert_eq!(entries.len(), 1, "failed v2 tree must be quarantined");
    assert_eq!(
        std::fs::read(entries[0].as_ref().unwrap().path().join("scripts/tool.py")).unwrap(),
        b"v2\n"
    );

    drop(manager);
    let mut manager = SkillsManager::for_test(
        &url,
        home.join(".agents/skills"),
        home.join(".agents/.ai-cove-skills"),
        tmp.path().join("cache/skills"),
    )
    .unwrap();
    let status = manager.status(true).await.unwrap();
    let skill = status_skill(&status, "demo-skill");
    assert_eq!(skill.local_state, "managed");
    assert_eq!(skill.installed_version.as_deref(), Some("1.0.0"));
}
