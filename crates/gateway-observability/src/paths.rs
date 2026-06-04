#![forbid(unsafe_code)]

use std::path::PathBuf;

#[must_use]
pub fn default_exchange_log_path() -> PathBuf {
    resolve_exchange_log_path(
        std::env::var("CLD_GATEWAY_LOG_PATH").ok().as_deref(),
        std::env::var("GATEWAY_HOME").ok().as_deref(),
    )
}

fn resolve_exchange_log_path(
    explicit_log_path: Option<&str>,
    gateway_home: Option<&str>,
) -> PathBuf {
    if let Some(path) = explicit_log_path {
        return PathBuf::from(path);
    }

    if let Some(home) = gateway_home {
        return PathBuf::from(home).join("logs").join("http-exchange.jsonl");
    }

    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".gateway")
        .join("logs")
        .join("http-exchange.jsonl")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_log_path_overrides_all() {
        let result = resolve_exchange_log_path(Some("/custom/log.jsonl"), Some("/some/home"));
        assert_eq!(result, PathBuf::from("/custom/log.jsonl"));
    }

    #[test]
    fn gateway_home_used_when_no_explicit_log_path() {
        let result = resolve_exchange_log_path(None, Some("/custom/home"));
        assert_eq!(
            result,
            PathBuf::from("/custom/home/logs/http-exchange.jsonl")
        );
    }

    #[test]
    fn falls_back_to_default_path_when_no_env_vars() {
        let result = resolve_exchange_log_path(None, None);
        assert!(
            result.to_string_lossy().contains(".gateway"),
            "expected .gateway in path: {result:?}"
        );
        assert!(
            result.to_string_lossy().ends_with("http-exchange.jsonl"),
            "expected http-exchange.jsonl suffix: {result:?}"
        );
    }
}
