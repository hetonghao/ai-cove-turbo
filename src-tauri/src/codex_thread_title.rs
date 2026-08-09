use std::{path::Path, process::Command};

#[cfg(target_os = "macos")]
const SQLITE_CLI: &str = "/usr/bin/sqlite3";
#[cfg(not(target_os = "macos"))]
const SQLITE_CLI: &str = "sqlite3";

pub(crate) fn is_codex_thread_id(thread_id: &str) -> bool {
    thread_id.len() == 36
        && thread_id.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

pub(crate) fn read_with_cli(executable: &Path, database: &Path, thread_id: &str) -> Option<String> {
    if !is_codex_thread_id(thread_id) || !database.is_file() {
        return None;
    }
    let query = format!(
        "SELECT COALESCE(NULLIF(name, ''), title) FROM threads WHERE id = '{thread_id}' LIMIT 1;"
    );
    let output = Command::new(executable)
        .arg("-readonly")
        .arg("-noheader")
        .arg(database)
        .arg(query)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let title = String::from_utf8(output.stdout).ok()?;
    let title = title.trim_end_matches(&['\r', '\n'][..]);
    (!title.is_empty()).then(|| title.to_owned())
}

pub(crate) async fn read(database: std::path::PathBuf, thread_id: String) -> Option<String> {
    if !is_codex_thread_id(&thread_id) {
        return None;
    }
    tokio::task::spawn_blocking(move || read_with_cli(Path::new(SQLITE_CLI), &database, &thread_id))
        .await
        .ok()
        .flatten()
}
