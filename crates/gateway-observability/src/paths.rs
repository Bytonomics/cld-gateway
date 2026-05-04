#![forbid(unsafe_code)]

use std::path::PathBuf;

#[must_use]
pub fn default_exchange_log_path() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".gateway")
        .join("logs")
        .join("http-exchange.jsonl")
}
