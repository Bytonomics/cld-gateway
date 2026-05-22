#![forbid(unsafe_code)]

use sha2::Digest as _;
use sha2::Sha256;
use std::path::{Path, PathBuf};

const KEYRING_SERVICE: &str = "Codex Auth";

#[must_use]
pub fn codex_home_dir() -> PathBuf {
    if let Ok(codex_home) = std::env::var("CODEX_HOME") {
        return PathBuf::from(codex_home);
    }
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".codex")
}

fn compute_store_key(codex_home: &Path) -> String {
    let canonical = codex_home
        .canonicalize()
        .unwrap_or_else(|_| codex_home.to_path_buf());
    let path_str = canonical.to_string_lossy();
    let mut hasher = Sha256::new();
    hasher.update(path_str.as_bytes());
    let digest = hasher.finalize();
    let hex = format!("{digest:x}");
    let truncated = hex.get(..16).unwrap_or(&hex);
    format!("cli|{truncated}")
}

#[must_use]
pub fn keyring_service() -> &'static str {
    KEYRING_SERVICE
}

#[must_use]
pub fn compute_default_key() -> (PathBuf, String) {
    let codex_home = codex_home_dir();
    let key = compute_store_key(&codex_home);
    (codex_home, key)
}

#[derive(Debug, thiserror::Error)]
pub enum KeyringAuthError {
    #[error("no auth found in keyring")]
    NotFound,
    #[error("keyring error")]
    Keyring(#[source] keyring::Error),
}

/// Loads the Codex CLI auth JSON blob from OS keyring (macOS Keychain, etc).
///
/// This matches the codex-rs login storage scheme:
/// - service: "Codex Auth"
/// - key: `cli|{sha256(codex_home_path)[:16]}`
///
/// # Errors
///
/// Returns `NotFound` if no key exists, or `Keyring` for other keyring failures.
pub fn load_codex_auth_json_from_keyring() -> Result<String, KeyringAuthError> {
    let (_, key) = compute_default_key();

    let entry = keyring::Entry::new(KEYRING_SERVICE, &key).map_err(KeyringAuthError::Keyring)?;
    match entry.get_password() {
        Ok(v) => Ok(v),
        Err(keyring::Error::NoEntry) => Err(KeyringAuthError::NotFound),
        Err(e) => Err(KeyringAuthError::Keyring(e)),
    }
}

/// Persists the Codex CLI auth JSON blob to OS keyring (macOS Keychain, etc).
///
/// This matches the codex-rs login storage scheme:
/// - service: "Codex Auth"
/// - key: `cli|{sha256(codex_home_path)[:16]}`
///
/// # Errors
///
/// Returns `Keyring` for any keyring failures.
pub fn save_codex_auth_json_to_keyring_key(key: &str, json: &str) -> Result<(), KeyringAuthError> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, key).map_err(KeyringAuthError::Keyring)?;
    entry
        .set_password(json)
        .map_err(KeyringAuthError::Keyring)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_store_key_is_stable() {
        // This is intentionally not asserting a specific hash value (platform-dependent
        // canonicalization), but it ensures we keep the expected prefix/length.
        let dir = tempfile::tempdir().expect("tempdir");
        let key = compute_store_key(dir.path());
        assert!(key.starts_with("cli|"));
        assert_eq!(key.len(), "cli|".len() + 16);
    }
}
