#![forbid(unsafe_code)]

use std::path::PathBuf;

#[must_use]
pub fn default_auth_json_path() -> PathBuf {
    if let Ok(codex_home) = std::env::var("CODEX_HOME") {
        return PathBuf::from(codex_home).join("auth.json");
    }

    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".codex").join("auth.json")
}
