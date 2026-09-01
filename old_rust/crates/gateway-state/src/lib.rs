// Crate: gateway-state
// Purpose: local persistence for gateway runtime state.
// Allowed deps: small local storage libs (rusqlite), dirs, serde.
// Not allowed: storing prompts, tokens, or raw request/response bodies outside the explicit
// conversation-state ledger/checkpoint model.

#![forbid(unsafe_code)]

mod conversation;
mod tool_calls;

use std::path::PathBuf;

pub use conversation::{
    BranchCheckpointRef, BranchCreateParams, BranchFingerprintSet, BranchMetadata,
    BranchSelectionAction, BranchSelectionInput, BranchSelectionResult, CanonicalLedgerEvent,
    ClaudeSessionMetadata, CommitOffshootCheckpointParams, CommitTurnParams,
    ConversationStateStore, ConversationTurnScope, OffshootOpenAiCheckpoint, OpenAiCheckpoint,
    ReconcileSnapshotParams, SparseCheckpoint, SparseCheckpointKind, TurnOpenAiCheckpoint,
};
pub use tool_calls::{StoredToolCall, ToolCallStore};

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("failed to resolve home directory")]
    NoHomeDir,
    #[error("sqlite error")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io error")]
    Io(#[from] std::io::Error),
    #[error("json error")]
    Json(#[from] serde_json::Error),
    #[error("state invariant error: {0}")]
    Invariant(String),
}

pub(crate) fn default_gateway_dir() -> Result<PathBuf, StateError> {
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
