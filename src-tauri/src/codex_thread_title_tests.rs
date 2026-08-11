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
fn reads_subagent_name_and_parent_through_the_read_only_cli() -> io::Result<()> {
    // Given: sqlite3 returns the display metadata selected for a subagent thread.
    let root = tempfile::tempdir()?;
    let cli = root.path().join("sqlite3");
    let arguments = root.path().join("arguments");
    fs::write(
        &cli,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nprintf '%s\\n' '[{{\"name\":\"Nash\",\"parent_name\":\"Turbo 主会话\",\"is_subagent\":1}}]'\n",
            arguments.display()
        ),
    )?;
    let mut permissions = fs::metadata(&cli)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&cli, permissions)?;

    let database = root.path().join("state_5.sqlite");
    fs::write(&database, [])?;
    // When: Turbo reads one known Codex thread without opening the database for writes.
    let info = read_with_cli(&cli, &database, "019fc8b1-a38c-7e70-9169-d6d76a7fcedc");

    // Then: the child nickname and readable parent name are returned as typed metadata.
    let info = info.expect("subagent metadata should be returned");
    assert_eq!(info.name.as_deref(), Some("Nash"));
    assert_eq!(info.parent_name.as_deref(), Some("Turbo 主会话"));
    assert!(info.is_subagent);
    let recorded = fs::read_to_string(arguments)?;
    assert!(recorded.contains("-readonly"));
    assert!(recorded.contains("-json"));
    assert!(recorded.contains(database.to_string_lossy().as_ref()));
    assert!(recorded.contains("agent_nickname"));
    assert!(recorded.contains("thread_spawn_edges"));
    assert!(recorded.contains("parent_name"));
    assert!(!recorded.contains("child.title"));
    assert!(!recorded.contains("parent.title"));
    Ok(())
}
