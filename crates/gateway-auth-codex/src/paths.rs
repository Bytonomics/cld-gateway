#![forbid(unsafe_code)]

use std::path::PathBuf;

#[must_use]
pub fn default_auth_json_path() -> PathBuf {
    if let Ok(path) = std::env::var("GATEWAY_AUTH_JSON_PATH") {
        return PathBuf::from(path);
    }

    if let Ok(gateway_home) = std::env::var("GATEWAY_HOME") {
        return PathBuf::from(gateway_home).join("auth.json");
    }

    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".gateway").join("auth.json")
}
