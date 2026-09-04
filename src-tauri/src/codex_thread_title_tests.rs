#![cfg(unix)]

use std::{fs, io, os::unix::fs::PermissionsExt, path::Path};

use crate::codex_thread_title::{
    ThreadInfoReadError, is_codex_thread_id, read_batch_with_cli, read_with_cli,
};

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
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\nhas_timeout=0\nfor arg in \"$@\"; do [ \"$arg\" = \".timeout 1500\" ] && has_timeout=1; done\nif [ \"$has_timeout\" -eq 0 ]; then printf '%s\\n' '[{{\"timeout\":1500}}]'; fi\nprintf '%s\\n' '[{{\"name\":\"Nash\",\"parent_name\":\"Turbo 主会话\",\"is_subagent\":1,\"model\":\"gpt-5.3-codex\"}}]'\n",
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
    assert_eq!(info.model.as_deref(), Some("gpt-5.3-codex"));
    let recorded = fs::read_to_string(arguments)?;
    assert!(recorded.contains("-readonly"));
    assert!(recorded.contains("-json"));
    assert!(recorded.contains(".timeout 1500"));
    assert!(recorded.contains(database.to_string_lossy().as_ref()));
    assert!(recorded.contains("agent_nickname"));
    assert!(recorded.contains("thread_spawn_edges"));
    assert!(recorded.contains("parent_name"));
    assert!(!recorded.contains("child.title"));
    assert!(!recorded.contains("parent.title"));
    Ok(())
}

#[test]
fn reads_multiple_thread_names_in_one_read_only_batch() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    let cli = root.path().join("sqlite3");
    fs::write(
        &cli,
        "#!/bin/sh\nhas_timeout=0\nfor arg in \"$@\"; do [ \"$arg\" = \".timeout 1500\" ] && has_timeout=1; done\nif [ \"$has_timeout\" -eq 0 ]; then printf '%s\\n' '[{\"timeout\":1500}]'; fi\nprintf '%s\\n' '[{\"thread_id\":\"019fc8b1-a38c-7e70-9169-d6d76a7fcedc\",\"name\":\"代码审查\",\"parent_name\":null,\"is_subagent\":0,\"model\":\"gpt-5.3-codex\"}]'\n",
    )?;
    let mut permissions = fs::metadata(&cli)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&cli, permissions)?;
    let database = root.path().join("state_5.sqlite");
    fs::write(&database, [])?;
    let ids = vec![
        "019fc8b1-a38c-7e70-9169-d6d76a7fcedc".to_owned(),
        "019fc8b1-a38c-7e70-9169-d6d76a7fcedd".to_owned(),
    ];

    let rows = read_batch_with_cli(&cli, &database, &ids)
        .map_err(|error| io::Error::other(format!("batch read failed: {error:?}")))?;

    assert_eq!(rows.len(), 1);
    let row = rows
        .first()
        .ok_or_else(|| io::Error::other("batch read returned no rows"))?;
    let first_id = ids
        .first()
        .ok_or_else(|| io::Error::other("test ids are empty"))?;
    assert_eq!(row.thread_id, *first_id);
    assert_eq!(row.info.name.as_deref(), Some("代码审查"));
    assert_eq!(row.info.model.as_deref(), Some("gpt-5.3-codex"));
    Ok(())
}

#[test]
fn rejects_invalid_batch_thread_ids_before_running_sqlite() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    let database = root.path().join("state_5.sqlite");
    fs::write(&database, [])?;
    let ids = vec!["thread-1".to_owned()];

    assert_eq!(
        read_batch_with_cli(Path::new("/missing/sqlite3"), &database, &ids),
        Err(ThreadInfoReadError::InvalidThreadId),
    );
    Ok(())
}
