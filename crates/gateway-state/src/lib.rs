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
            INSERT OR REPLACE INTO tool_calls(call_id, tool_name, created_at, request_id)
            VALUES (?1, ?2, ?3, ?4)
            ",
            (call_id, tool_name, now, request_id),
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
                created_at INTEGER NOT NULL,
                request_id TEXT
            );
            ",
        )?;
        Ok(conn)
    }
}

fn default_gateway_dir() -> Result<PathBuf, StateError> {
    let home = dirs::home_dir().ok_or(StateError::NoHomeDir)?;
    Ok(home.join(".gateway"))
}

fn default_tool_calls_db_path() -> Result<PathBuf, StateError> {
    Ok(default_gateway_dir()?
        .join("state")
        .join("tool_calls.sqlite"))
}
