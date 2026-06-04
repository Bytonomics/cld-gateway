// Crate: gateway-state
// Purpose: minimal local persistence for gateway runtime state (IDs/correlation only).
// Allowed deps: small local storage libs (rusqlite), dirs.
// Not allowed: storing prompts, tokens, or request/response bodies.

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("failed to resolve home directory")]
    NoHomeDir,
    #[error("sqlite error")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io error")]
    Io(#[from] std::io::Error),
}

#[derive(Clone)]
pub struct ToolCallStore {
    path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredToolCall {
    pub tool_name: String,
    pub tool_kind: String,
}

impl Default for ToolCallStore {
    fn default() -> Self {
        Self {
            path: default_tool_calls_db_path()
                .unwrap_or_else(|_| PathBuf::from("tool_calls.sqlite")),
        }
    }
}

impl ToolCallStore {
    #[must_use]
    pub fn new(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
        }
    }

    /// # Errors
    /// Returns an error if the DB cannot be opened or initialized.
    pub fn ensure_schema(&self) -> Result<(), StateError> {
        self.open_and_init().map(|_| ())
    }

    /// # Errors
    /// Returns an error if the DB cannot be opened/initialized or the insert fails.
    pub fn record_tool_call(
        &self,
        call_id: &str,
        tool_name: &str,
        tool_kind: &str,
        request_id: Option<&str>,
    ) -> Result<(), StateError> {
        let conn = self.open_and_init()?;
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .min(i64::MAX as u64);
        let now = i64::try_from(secs).unwrap_or(i64::MAX);

        conn.execute(
            r"
            INSERT OR REPLACE INTO tool_calls(call_id, tool_name, tool_kind, created_at, request_id)
            VALUES (?1, ?2, ?3, ?4, ?5)
            ",
            (call_id, tool_name, tool_kind, now, request_id),
        )?;
        Ok(())
    }

    /// # Errors
    /// Returns an error if the DB cannot be opened/initialized or the query fails.
    pub fn tool_call_exists(&self, call_id: &str) -> Result<bool, StateError> {
        let conn = self.open_and_init()?;
        let mut stmt = conn.prepare("SELECT 1 FROM tool_calls WHERE call_id = ?1 LIMIT 1")?;
        let mut rows = stmt.query([call_id])?;
        Ok(rows.next()?.is_some())
    }

    /// # Errors
    /// Returns an error if the DB cannot be opened/initialized or the query fails.
    pub fn get_tool_call(&self, call_id: &str) -> Result<Option<StoredToolCall>, StateError> {
        let conn = self.open_and_init()?;
        let mut stmt =
            conn.prepare("SELECT tool_name, tool_kind FROM tool_calls WHERE call_id = ?1 LIMIT 1")?;
        let mut rows = stmt.query([call_id])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        Ok(Some(StoredToolCall {
            tool_name: row.get(0)?,
            tool_kind: row.get(1)?,
        }))
    }

    fn open_and_init(&self) -> Result<rusqlite::Connection, StateError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = rusqlite::Connection::open(&self.path)?;
        conn.execute_batch(
            r"
            CREATE TABLE IF NOT EXISTS tool_calls
            (
                call_id    TEXT PRIMARY KEY,
                tool_name  TEXT NOT NULL,
                tool_kind  TEXT NOT NULL DEFAULT 'function_call',
                created_at INTEGER NOT NULL,
                request_id TEXT
            );
            ",
        )?;
        ensure_tool_kind_column(&conn)?;
        Ok(conn)
    }
}

fn ensure_tool_kind_column(conn: &rusqlite::Connection) -> Result<(), StateError> {
    let mut stmt = conn.prepare("PRAGMA table_info(tool_calls)")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    for row in rows {
        if row? == "tool_kind" {
            return Ok(());
        }
    }
    conn.execute(
        "ALTER TABLE tool_calls ADD COLUMN tool_kind TEXT NOT NULL DEFAULT 'function_call'",
        [],
    )?;
    Ok(())
}

fn default_gateway_dir() -> Result<PathBuf, StateError> {
    if let Ok(gateway_home) = std::env::var("GATEWAY_HOME") {
        return Ok(PathBuf::from(gateway_home));
    }
    let home = dirs::home_dir().ok_or(StateError::NoHomeDir)?;
    Ok(home.join(".gateway"))
}

fn default_tool_calls_db_path() -> Result<PathBuf, StateError> {
    if let Ok(path) = std::env::var("CLD_GATEWAY_STATE_DB_PATH") {
        return Ok(PathBuf::from(path));
    }
    Ok(default_gateway_dir()?
        .join("state")
        .join("tool_calls.sqlite"))
}

#[cfg(test)]
fn resolve_tool_calls_db_path(
    explicit_db_path: Option<&str>,
    gateway_home: Option<&str>,
) -> Result<PathBuf, StateError> {
    if let Some(path) = explicit_db_path {
        return Ok(PathBuf::from(path));
    }
    if let Some(home) = gateway_home {
        return Ok(PathBuf::from(home).join("state").join("tool_calls.sqlite"));
    }
    let home = dirs::home_dir().ok_or(StateError::NoHomeDir)?;
    Ok(home
        .join(".gateway")
        .join("state")
        .join("tool_calls.sqlite"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db_path(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("gateway_state_{name}_{nanos}.sqlite"))
    }

    #[test]
    fn records_and_reads_tool_kind() {
        let path = temp_db_path("tool_kind");
        let store = ToolCallStore::new(&path);
        store
            .record_tool_call("call_1", "apply_patch", "custom_tool_call", Some("rid_1"))
            .expect("record");

        let stored = store
            .get_tool_call("call_1")
            .expect("read")
            .expect("stored call");
        assert_eq!(stored.tool_name, "apply_patch");
        assert_eq!(stored.tool_kind, "custom_tool_call");
    }

    #[test]
    fn migrates_existing_tool_calls_without_kind() {
        let path = temp_db_path("migration");
        let conn = rusqlite::Connection::open(&path).expect("open sqlite");
        conn.execute_batch(
            r"
            CREATE TABLE tool_calls
            (
                call_id    TEXT PRIMARY KEY,
                tool_name  TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                request_id TEXT
            );
            INSERT INTO tool_calls(call_id, tool_name, created_at, request_id)
            VALUES ('call_1', 'Read', 1, 'rid_1');
            ",
        )
        .expect("create old schema");
        drop(conn);

        let store = ToolCallStore::new(&path);
        let stored = store
            .get_tool_call("call_1")
            .expect("read")
            .expect("stored call");
        assert_eq!(stored.tool_kind, "function_call");
    }

    #[test]
    fn explicit_db_path_overrides_all() {
        let result = resolve_tool_calls_db_path(Some("/explicit/db.sqlite"), Some("/some/home"))
            .expect("path");
        assert_eq!(result, PathBuf::from("/explicit/db.sqlite"));
    }

    #[test]
    fn gateway_home_used_when_no_explicit_db_path() {
        let result = resolve_tool_calls_db_path(None, Some("/custom/gateway")).expect("path");
        assert_eq!(
            result,
            PathBuf::from("/custom/gateway/state/tool_calls.sqlite")
        );
    }

    #[test]
    fn falls_back_to_default_db_path_when_no_env_vars() {
        let result = resolve_tool_calls_db_path(None, None).expect("path");
        assert!(
            result.to_string_lossy().contains(".gateway"),
            "expected .gateway in path: {result:?}"
        );
        assert!(
            result.to_string_lossy().ends_with("tool_calls.sqlite"),
            "expected tool_calls.sqlite suffix: {result:?}"
        );
    }
}
