#![cfg(unix)]

use std::{fs, io, os::unix::fs::PermissionsExt};

use crate::codex_thread_title::{is_codex_thread_id, read_with_cli};

#[test]
fn accepts_only_codex_uuid_thread_ids() {
    assert!(is_codex_thread_id("019fc8b1-a38c-7e70-9169-d6d76a7fcedc"));
    assert!(!is_codex_thread_id("thread-12345678-alpha"));
    assert!(!is_codex_thread_id(
        "019fc8b1-a38c-7e70-9169-d6d76a7fcedc' OR 1=1"
    ));
}

#[test]
fn reads_the_requested_title_through_the_read_only_cli() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    let cli = root.path().join("sqlite3");
    let arguments = root.path().join("arguments");
    fs::write(
        &cli,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nprintf '连接面板优化\\n'\n",
            arguments.display()
        ),
    )?;
    let mut permissions = fs::metadata(&cli)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&cli, permissions)?;

    let database = root.path().join("state_5.sqlite");
    fs::write(&database, [])?;
    let title = read_with_cli(&cli, &database, "019fc8b1-a38c-7e70-9169-d6d76a7fcedc");

    assert_eq!(title.as_deref(), Some("连接面板优化"));
    let recorded = fs::read_to_string(arguments)?;
    assert!(recorded.contains("-readonly"));
    assert!(recorded.contains(database.to_string_lossy().as_ref()));
    assert!(recorded.contains("COALESCE(NULLIF(name, ''), title)"));
    Ok(())
}
