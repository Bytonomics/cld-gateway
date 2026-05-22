#![forbid(unsafe_code)]

use crate::{CodexAuthError, oauth::RefreshResponse};
use std::io::Write as _;
use std::path::Path;
use tempfile::NamedTempFile;

pub fn apply_refreshed_tokens(
    auth_value: &mut serde_json::Value,
    refreshed: &RefreshResponse,
) -> Result<(), CodexAuthError> {
    let tokens = auth_value
        .get_mut("tokens")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or(CodexAuthError::MissingField("tokens"))?;

    let access = refreshed
        .access_token
        .as_ref()
        .ok_or(CodexAuthError::RefreshUnexpectedResponse)?;
    tokens.insert(
        "access_token".to_string(),
        serde_json::Value::String(access.clone()),
    );

    if let Some(rt) = refreshed.refresh_token.as_ref() {
        tokens.insert(
            "refresh_token".to_string(),
            serde_json::Value::String(rt.clone()),
        );
    }

    if let Some(idt) = refreshed.id_token.as_ref() {
        tokens.insert(
            "id_token".to_string(),
            serde_json::Value::String(idt.clone()),
        );
    }

    Ok(())
}

pub fn atomic_write_json(path: &Path, value: &serde_json::Value) -> Result<(), CodexAuthError> {
    let dir = path
        .parent()
        .ok_or(CodexAuthError::MissingField("auth.json parent dir"))?;
    std::fs::create_dir_all(dir)?;

    let bytes = serde_json::to_vec_pretty(value)?;

    let mut tmp = NamedTempFile::new_in(dir)?;
    tmp.write_all(&bytes)?;
    tmp.flush()?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}
