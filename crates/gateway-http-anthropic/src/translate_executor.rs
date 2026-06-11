#![forbid(unsafe_code)]

use gateway_auth_codex::CodexCredentials;
use gateway_backend_codex::client::CodexBackendClient;
use serde_json::json;

/// Runtime context passed to executor functions.
pub struct ExecutorRuntime {
    pub credentials: Option<CodexCredentials>,
    pub backend_client: CodexBackendClient,
    /// Current model being used in this session
    pub current_model: Option<String>,
    /// Gateway's local session/thread information
    pub session_info: SessionInfo,
    /// Gateway binary version (from `CARGO_PKG_VERSION`)
    pub gateway_version: &'static str,
    /// Path to the gateway config file being used
    pub config_path: Option<String>,
    /// The resolved model name after model resolution
    pub resolved_model: Option<String>,
    /// Current working directory of the gateway process
    pub current_dir: Option<String>,
    /// Reasoning effort from request (if present in client metadata)
    pub reasoning_effort: Option<String>,
}

/// Session information for status display
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub thread_id: Option<String>,
    pub thread_name: Option<String>,
    pub account_display: Option<String>,
}

/// Post-result wrapper function signature: takes executor JSON and packaged body, returns final output
type PostResultFn = fn(&serde_json::Value, &str) -> String;

/// Registry of translated command names that have executor functions.
/// Allows future translated commands to be registered without code branching changes.
static COMMAND_EXECUTOR_NAMES: &[&str] = &["status"];

/// Registry of translated commands and their post-result wrapper functions.
/// Maps normalized command name to post-result function.
/// Allows future translated commands to customize how executor output is wrapped with packaged prompt text.
static COMMAND_POST_RESULTS: &[(&str, PostResultFn)] =
    &[("status", post_result_for_translated_command)];

/// Executes a translated command if one is present in the metadata.
///
/// Returns the executor JSON result if a translated command was found and executed.
/// Returns None if no translated command was detected.
/// Returns an error if the command was found but execution failed (explicit, not silent degradation).
pub async fn execute_translated_command(
    command_name: Option<&str>,
    runtime: &ExecutorRuntime,
) -> Result<Option<serde_json::Value>, String> {
    let Some(cmd) = command_name else {
        return Ok(None);
    };

    // Normalize: remove leading slash and whitespace.
    let normalized = cmd.trim().trim_start_matches('/');

    // Check if command exists in the registry
    if !COMMAND_EXECUTOR_NAMES.contains(&normalized) {
        return Ok(None);
    }

    // Dispatch to the appropriate async executor
    let result = match normalized {
        "status" => execute_status_command(runtime).await,
        _ => return Ok(None),
    };

    Ok(Some(result))
}

/// Executes the /status command.
/// Builds a Gateway-owned status document with local session info and optional usage data.
/// Returns immediately with local state; usage data enrichment is non-blocking.
async fn execute_status_command(runtime: &ExecutorRuntime) -> serde_json::Value {
    let base_url = runtime.backend_client.base_url();
    let account_id = runtime
        .credentials
        .as_ref()
        .map_or_else(|| "unavailable".to_string(), |c| c.account_id.clone());

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());

    // Build local status immediately (non-blocking).
    let mut status = json!({
        "status_type": "gateway_status",
        "generated_at": timestamp,
        "gateway": {
            "version": runtime.gateway_version,
            "config_path": runtime.config_path.as_deref(),
            "current_dir": runtime.current_dir.as_deref(),
        },
        "session": {
            "thread_id": runtime.session_info.thread_id.as_deref(),
            "thread_name": runtime.session_info.thread_name.as_deref(),
            "account_display": runtime.session_info.account_display.as_deref(),
        },
        "model": {
            "requested": runtime.current_model.as_deref(),
            "resolved": runtime.resolved_model.as_deref()
                .or(runtime.current_model.as_deref()),
            "reasoning_effort": runtime.reasoning_effort.as_deref(),
        },
        "provider": {
            "base_url": base_url.trim_end_matches('/'),
        },
        "auth": {
            "account_id": account_id,
        },
        "usage_state": "pending",
        "plan_type": null,
        "rate_limits": null,
        "spend_control": null,
        "usage_raw": null,
    });

    // Attempt to enrich with live usage data.
    // Errors are captured in the status document, not escalated as executor failure.
    if let Ok(usage_data) = fetch_live_usage_data(runtime).await {
        // Surface plan_type at top level
        if let Some(plan_type) = usage_data.get("plan_type") {
            status["plan_type"] = plan_type.clone();
        }
        // Normalize rate-limit summary
        status["rate_limits"] = normalize_rate_limits(&usage_data);
        // Surface spend-control summary
        if let Some(spend) = usage_data.get("spend_control") {
            status["spend_control"] = normalize_spend_control(spend);
        }
        // Keep raw blob for consumers that need full detail
        status["usage_raw"] = usage_data;
        status["usage_state"] = "current".into();
    } else {
        status["usage_state"] = "stale_or_unavailable".into();
    }

    status
}

/// Fetches live usage/rate-limit data from the upstream Codex API.
/// Errors are captured but do not cause the executor to fail.
async fn fetch_live_usage_data(runtime: &ExecutorRuntime) -> Result<serde_json::Value, String> {
    let Some(creds) = runtime.credentials.as_ref() else {
        return Err("credentials_unavailable".to_string());
    };

    let base_url = runtime.backend_client.base_url();
    let url = format!("{}/api/codex/usage", base_url.trim_end_matches('/'));
    let authorization = format!("Bearer {}", creds.access_token.expose());
    let account_id = &creds.account_id;

    let http_client = runtime.backend_client.http_client();

    let response = http_client
        .get(&url)
        .map_err(|err| format!("usage_fetch_policy_error: {err}"))?
        .header("Authorization", &authorization)
        .header("chatgpt-account-id", account_id)
        .execute()
        .await
        .map_err(|err| format!("usage_fetch_transport_error: {err}"))?;

    let status = response.status();
    if !status.is_success() {
        return Err(format!("usage_fetch_status_{}", status.as_u16()));
    }

    let body = response
        .text()
        .await
        .map_err(|err| format!("usage_fetch_body_error: {err}"))?;

    serde_json::from_str::<serde_json::Value>(&body)
        .map_err(|err| format!("usage_fetch_parse_error: {err}"))
}

/// Normalizes rate-limit data from the upstream usage response into stable Gateway-owned fields.
fn normalize_rate_limits(usage: &serde_json::Value) -> serde_json::Value {
    let mut limits = json!({});

    // Primary rate limit
    if let Some(rl) = usage.get("rate_limit") {
        limits["primary"] = json!({
            "allowed": rl.get("allowed"),
            "limit_reached": rl.get("limit_reached"),
        });
        if let Some(pw) = rl.get("primary_window") {
            limits["primary"]["used_percent"] = pw.get("used_percent").cloned().unwrap_or_default();
            limits["primary"]["reset_at"] = pw.get("reset_at").cloned().unwrap_or_default();
            limits["primary"]["window_seconds"] =
                pw.get("limit_window_seconds").cloned().unwrap_or_default();
        }
        if let Some(sw) = rl.get("secondary_window") {
            limits["secondary"] = json!({
                "used_percent": sw.get("used_percent"),
                "reset_at": sw.get("reset_at"),
                "window_seconds": sw.get("limit_window_seconds"),
            });
        }
    }

    // Additional per-limit entries
    if let Some(additional) = usage
        .get("additional_rate_limits")
        .and_then(|v| v.as_array())
    {
        let entries: Vec<serde_json::Value> = additional
            .iter()
            .filter_map(|entry| {
                let name = entry.get("limit_name")?.as_str()?;
                let rl = entry.get("rate_limit")?;
                let pw = rl.get("primary_window")?;
                Some(json!({
                    "limit_name": name,
                    "allowed": rl.get("allowed"),
                    "limit_reached": rl.get("limit_reached"),
                    "used_percent": pw.get("used_percent"),
                    "reset_at": pw.get("reset_at"),
                    "window_seconds": pw.get("limit_window_seconds"),
                }))
            })
            .collect();
        if !entries.is_empty() {
            limits["additional"] = serde_json::Value::Array(entries);
        }
    }

    limits
}

/// Normalizes spend-control data from the upstream usage response.
fn normalize_spend_control(spend: &serde_json::Value) -> serde_json::Value {
    let mut result = json!({
        "reached": spend.get("reached"),
    });
    if let Some(limit) = spend.get("individual_limit") {
        result["individual_limit"] = json!({
            "source": limit.get("source"),
            "limit": limit.get("limit"),
            "used": limit.get("used"),
            "remaining": limit.get("remaining"),
            "used_percent": limit.get("used_percent"),
            "remaining_percent": limit.get("remaining_percent"),
            "reset_at": limit.get("reset_at"),
        });
    }
    result
}

/// Wraps executor JSON result with packaged command instructions if provided.
///
/// If `packaged_body` is non-empty after trimming:
///   Returns: `"{packaged_body}\n\n{executor_json_pretty}"`
///
/// If `packaged_body` is empty:
///   Returns: `"{executor_json_pretty}"` only
///
/// This ensures empty packaged prompt does not inject synthetic scaffolding.
pub fn post_result_for_translated_command(
    executor_json: &serde_json::Value,
    packaged_body: &str,
) -> String {
    let packaged_trimmed = packaged_body.trim();
    let json_str =
        serde_json::to_string_pretty(executor_json).unwrap_or_else(|_| executor_json.to_string());

    if packaged_trimmed.is_empty() {
        // No packaged instructions, return JSON only (no synthetic scaffolding)
        json_str
    } else {
        // Prepend packaged instructions to JSON
        format!("{packaged_trimmed}\n\n{json_str}")
    }
}

/// Returns the post-result wrapper function for a translated command, if registered.
pub fn get_post_result_function(command_name: &str) -> Option<PostResultFn> {
    let normalized = command_name.trim().trim_start_matches('/');
    COMMAND_POST_RESULTS
        .iter()
        .find(|(name, _)| *name == normalized)
        .map(|(_, func)| *func)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_runtime(with_creds: bool) -> ExecutorRuntime {
        let creds = if with_creds {
            Some(gateway_auth_codex::CodexCredentials {
                access_token: gateway_core::Secret::new("test-token".to_string()),
                account_id: "test-account-123".to_string(),
            })
        } else {
            None
        };
        ExecutorRuntime {
            credentials: creds,
            backend_client: CodexBackendClient::default(),
            current_model: Some("claude-sonnet-4-20250514".to_string()),
            session_info: SessionInfo {
                thread_id: None,
                thread_name: None,
                account_display: Some("test-account-123".to_string()),
            },
            gateway_version: "0.0.0-test",
            config_path: Some("/tmp/test-config.yml".to_string()),
            resolved_model: Some("gpt-4o".to_string()),
            current_dir: Some("/tmp/test-workdir".to_string()),
            reasoning_effort: Some("high".to_string()),
        }
    }

    #[tokio::test]
    async fn execute_translated_command_dispatches_status() {
        let runtime = test_runtime(false);
        let result = execute_translated_command(Some("/status"), &runtime).await;
        let json = result.expect("no error").expect("executor found");
        assert_eq!(json["status_type"], "gateway_status");
    }

    #[tokio::test]
    async fn execute_translated_command_returns_none_for_unknown() {
        let runtime = test_runtime(false);
        let result = execute_translated_command(Some("/nonexistent"), &runtime).await;
        assert!(result.expect("no error").is_none());
    }

    #[tokio::test]
    async fn execute_translated_command_returns_none_for_no_command() {
        let runtime = test_runtime(false);
        let result = execute_translated_command(None, &runtime).await;
        assert!(result.expect("no error").is_none());
    }

    #[tokio::test]
    async fn status_executor_returns_structured_json_with_all_fields() {
        let runtime = test_runtime(false);
        let json = execute_status_command(&runtime).await;

        // Timestamp
        assert!(json["generated_at"].as_u64().unwrap() > 0);

        // Gateway fields
        assert_eq!(json["gateway"]["version"], "0.0.0-test");
        assert_eq!(json["gateway"]["config_path"], "/tmp/test-config.yml");
        assert_eq!(json["gateway"]["current_dir"], "/tmp/test-workdir");

        // Model fields
        assert_eq!(json["model"]["requested"], "claude-sonnet-4-20250514");
        assert_eq!(json["model"]["resolved"], "gpt-4o");
        assert_eq!(json["model"]["reasoning_effort"], "high");

        // Provider fields
        assert!(json["provider"]["base_url"].is_string());

        // Auth fields (no creds → "unavailable")
        assert_eq!(json["auth"]["account_id"], "unavailable");

        // Usage degraded without creds
        assert_eq!(json["usage_state"], "stale_or_unavailable");
        assert!(json["plan_type"].is_null());
        assert!(json["rate_limits"].is_null());
        assert!(json["spend_control"].is_null());
    }

    #[tokio::test]
    async fn status_executor_with_creds_shows_account_id() {
        let runtime = test_runtime(true);
        let json = execute_status_command(&runtime).await;
        assert_eq!(json["auth"]["account_id"], "test-account-123");
        // Usage fetch will fail (no real backend), but that's graceful degradation
        assert_eq!(json["usage_state"], "stale_or_unavailable");
    }

    #[test]
    fn post_result_with_empty_packaged_body_returns_json_only() {
        let json = serde_json::json!({"status_type": "gateway_status"});
        let result = post_result_for_translated_command(&json, "");
        // Should contain the JSON content but no prepended instructions
        assert!(result.contains("gateway_status"));
        assert!(!result.contains("Execute"));
        // Should NOT start with any instruction text — just JSON
        assert!(result.trim_start().starts_with('{'));
    }

    #[test]
    fn post_result_with_nonempty_packaged_body_prepends_instructions() {
        let json = serde_json::json!({"status_type": "gateway_status"});
        let result = post_result_for_translated_command(&json, "Show this status info clearly.");
        assert!(result.starts_with("Show this status info clearly."));
        assert!(result.contains("gateway_status"));
    }

    #[test]
    fn get_post_result_function_finds_status() {
        assert!(get_post_result_function("status").is_some());
        assert!(get_post_result_function("/status").is_some());
    }

    #[test]
    fn get_post_result_function_returns_none_for_unknown() {
        assert!(get_post_result_function("nonexistent").is_none());
    }

    #[test]
    fn missing_executor_for_classified_translate_is_distinguishable() {
        // When a command is classified Translate but has no executor,
        // execute_translated_command returns Ok(None). The handler in lib.rs
        // treats this as an explicit error for classified-Translate commands.
        // This test verifies the registry lookup behavior.
        assert!(COMMAND_EXECUTOR_NAMES.contains(&"status"));
        assert!(!COMMAND_EXECUTOR_NAMES.contains(&"nonexistent"));
    }
}
