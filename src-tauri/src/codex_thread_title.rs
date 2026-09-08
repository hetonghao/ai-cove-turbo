use std::path::Path;

use serde::{Deserialize, Serialize};

#[cfg(target_os = "macos")]
const SQLITE_CLI: &str = "/usr/bin/sqlite3";
#[cfg(not(target_os = "macos"))]
const SQLITE_CLI: &str = "sqlite3";

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CodexThreadInfo {
    pub(crate) name: Option<String>,
    pub(crate) parent_name: Option<String>,
    pub(crate) is_subagent: bool,
    pub(crate) model: Option<String>,
}

#[derive(Deserialize)]
struct CliThreadInfo {
    name: Option<String>,
    parent_name: Option<String>,
    is_subagent: i64,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ThreadInfoRow {
    pub(crate) thread_id: String,
    pub(crate) info: CodexThreadInfo,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ThreadInfoReadError {
    InvalidThreadId,
    DatabaseMissing,
    CommandFailed,
    InvalidOutput,
    Timeout,
}

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

pub(crate) fn read_with_cli(
    executable: &Path,
    database: &Path,
    thread_id: &str,
) -> Option<CodexThreadInfo> {
    if !is_codex_thread_id(thread_id) || !database.is_file() {
        return None;
    }
    let query = format!(
        r"
SELECT
    CASE
        WHEN child.thread_source = 'subagent'
            THEN COALESCE(NULLIF(child.agent_nickname, ''), NULLIF(child.name, ''))
        ELSE NULLIF(child.name, '')
    END AS name,
    CASE
        WHEN child.thread_source = 'subagent'
            THEN NULLIF(parent.name, '')
        ELSE NULL
    END AS parent_name,
    CASE WHEN child.thread_source = 'subagent' THEN 1 ELSE 0 END AS is_subagent,
    NULLIF(child.model, '') AS model
FROM threads AS child
LEFT JOIN thread_spawn_edges AS edge ON edge.child_thread_id = child.id
LEFT JOIN threads AS parent ON parent.id = edge.parent_thread_id
WHERE child.id = '{thread_id}'
LIMIT 1;
"
    );
    let output = crate::process_command(executable)
        .arg("-readonly")
        .arg("-json")
        .arg("-cmd")
        .arg(".timeout 1500")
        .arg(database)
        .arg(query)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let row = serde_json::from_slice::<Vec<CliThreadInfo>>(&output.stdout)
        .ok()?
        .into_iter()
        .next()?;
    Some(CodexThreadInfo {
        name: row.name,
        parent_name: row.parent_name,
        is_subagent: row.is_subagent != 0,
        model: row.model,
    })
}

pub(crate) fn read_batch_with_cli(
    executable: &Path,
    database: &Path,
    thread_ids: &[String],
) -> Result<Vec<ThreadInfoRow>, ThreadInfoReadError> {
    if thread_ids
        .iter()
        .any(|thread_id| !is_codex_thread_id(thread_id))
    {
        return Err(ThreadInfoReadError::InvalidThreadId);
    }
    if !database.is_file() {
        return Err(ThreadInfoReadError::DatabaseMissing);
    }
    if thread_ids.is_empty() {
        return Ok(Vec::new());
    }
    let ids = thread_ids
        .iter()
        .map(|thread_id| format!("'{thread_id}'"))
        .collect::<Vec<_>>()
        .join(",");
    let query = format!(
        r"
SELECT
    child.id AS thread_id,
    CASE
        WHEN child.thread_source = 'subagent'
            THEN COALESCE(NULLIF(child.agent_nickname, ''), NULLIF(child.name, ''))
        ELSE NULLIF(child.name, '')
    END AS name,
    CASE
        WHEN child.thread_source = 'subagent'
            THEN NULLIF(parent.name, '')
        ELSE NULL
    END AS parent_name,
    CASE WHEN child.thread_source = 'subagent' THEN 1 ELSE 0 END AS is_subagent,
    NULLIF(child.model, '') AS model
FROM threads AS child
LEFT JOIN thread_spawn_edges AS edge ON edge.child_thread_id = child.id
LEFT JOIN threads AS parent ON parent.id = edge.parent_thread_id
WHERE child.id IN ({ids});
"
    );
    let output = crate::process_command(executable)
        .arg("-readonly")
        .arg("-json")
        .arg("-cmd")
        .arg(".timeout 1500")
        .arg(database)
        .arg(query)
        .output()
        .map_err(|_| ThreadInfoReadError::CommandFailed)?;
    if !output.status.success() {
        return Err(ThreadInfoReadError::CommandFailed);
    }
    let rows = serde_json::from_slice::<Vec<CliBatchThreadInfo>>(&output.stdout)
        .map_err(|_| ThreadInfoReadError::InvalidOutput)?;
    Ok(rows
        .into_iter()
        .map(|row| ThreadInfoRow {
            thread_id: row.thread_id,
            info: CodexThreadInfo {
                name: row.name,
                parent_name: row.parent_name,
                is_subagent: row.is_subagent != 0,
                model: row.model,
            },
        })
        .collect())
}

#[derive(Deserialize)]
struct CliBatchThreadInfo {
    thread_id: String,
    name: Option<String>,
    parent_name: Option<String>,
    is_subagent: i64,
    #[serde(default)]
    model: Option<String>,
}

pub(crate) async fn read_batch(
    database: std::path::PathBuf,
    thread_ids: Vec<String>,
) -> Result<Vec<ThreadInfoRow>, ThreadInfoReadError> {
    tokio::task::spawn_blocking(move || {
        read_batch_with_cli(Path::new(SQLITE_CLI), &database, &thread_ids)
    })
    .await
    .map_err(|_| ThreadInfoReadError::CommandFailed)?
}

pub(crate) async fn read(
    database: std::path::PathBuf,
    thread_id: String,
) -> Option<CodexThreadInfo> {
    if !is_codex_thread_id(&thread_id) {
        return None;
    }
    tokio::task::spawn_blocking(move || read_with_cli(Path::new(SQLITE_CLI), &database, &thread_id))
        .await
        .ok()
        .flatten()
}
