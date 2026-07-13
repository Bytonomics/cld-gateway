// Crate: gateway-http-anthropic
// Purpose: Anthropic-compatible HTTP surface (routes, request parsing, translation).
// Allowed deps: gateway-core, gateway-auth-codex, gateway-backend-codex, gateway-observability.
// Not allowed: direct auth file IO (must go through gateway-auth-codex).

#![forbid(unsafe_code)]

use axum::Json;
use axum::body::{Bytes, to_bytes};
use axum::extract::Request;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::{get, post};
use eventsource_stream::Eventsource as _;
use futures_util::StreamExt as _;
use gateway_backend_codex::client::BackendError;
use gateway_backend_codex::types::{CodexToolCall, CodexToolCallKind};
use gateway_core::RequestId;
use gateway_core::config::{
    GatewayConfig, ModelResolution, OpenAiIncrementalTransportMode,
    load_gateway_config_default_path, resolve_model, service_tier_for_config,
};
use gateway_net::{GatewayHttpClient, GatewayNetworkPolicy};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::{info, warn};
use uuid::Uuid;

mod claude_code_context;
mod claude_code_inclusion;
mod context_management;
mod sse_bridge;
mod tool_arg_policy;
mod translate;
mod translate_executor;
mod types;

use crate::claude_code_context::normalize_claude_code_context;
use crate::context_management::{ContextManagementReport, ContextManager};
use crate::translate::{ToolTranslationContext, translate_request_with_context};
use crate::translate_executor::{ExecutorRuntime, execute_translated_command};
use crate::types::AnthropicMessagesRequest;
use gateway_state::{
    BranchFingerprintSet, BranchMetadata, BranchSelectionAction, BranchSelectionInput,
    CommitTurnParams, ConversationStateStore, ConversationTurnScope, ReconcileSnapshotParams,
    ToolCallStore,
};
use sha2::{Digest, Sha256};

#[derive(Clone, Default)]
pub struct AppState {
    auth: gateway_auth_codex::CodexAuthManager,
    backend: gateway_backend_codex::client::CodexBackendClient,
    tool_calls: ToolCallStore,
    conversation_state: ConversationStateStore,
    gateway_config: GatewayConfig,
    claude_gateway_settings_path: Option<PathBuf>,
    #[cfg(test)]
    auth_json_path: Option<PathBuf>,
}

impl AppState {
    /// # Errors
    ///
    /// Returns an error if the gateway config file exists but cannot be read or parsed.
    pub fn from_env() -> Result<Self, gateway_core::config::GatewayConfigError> {
        let gateway_config = load_gateway_config_default_path()?;
        let http = http_client_for_config(&gateway_config);
        let backend = backend_client_from_env(http.clone());
        let auth = gateway_auth_codex::CodexAuthManager::default().with_http_client(http.clone());
        Ok(Self {
            auth,
            backend,
            conversation_state: conversation_state_store_for_config(&gateway_config),
            claude_gateway_settings_path: Some(default_claude_gateway_settings_path()),
            gateway_config,
            ..Self::default()
        })
    }

    #[cfg(test)]
    #[must_use]
    fn with_claude_gateway_settings_path(mut self, path: PathBuf) -> Self {
        self.claude_gateway_settings_path = Some(path);
        self
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct ClaudeGatewaySettings {
    models: Vec<ClaudeGatewayModel>,
    env: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize)]
struct ClaudeGatewayModel {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct ClaudeGatewayModelsResponseItem {
    id: String,
    #[serde(rename = "type")]
    item_type: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
}

fn http_client_for_config(config: &GatewayConfig) -> GatewayHttpClient {
    let mut policy = GatewayNetworkPolicy::default();
    policy.extend_allowed_hosts(&config.network.allowed_hosts);
    GatewayHttpClient::new(policy)
}

fn backend_client_from_env(
    http: GatewayHttpClient,
) -> gateway_backend_codex::client::CodexBackendClient {
    let client =
        gateway_backend_codex::client::CodexBackendClient::default().with_http_client(http);
    match backend_request_timeout_from_env() {
        Some(timeout) => client.with_request_timeout(timeout),
        None => client,
    }
}

fn backend_request_timeout_from_env() -> Option<Duration> {
    let raw = std::env::var("GATEWAY_BACKEND_REQUEST_TIMEOUT_SECS").ok()?;
    match raw.parse::<u64>() {
        Ok(0) => None,
        Ok(seconds) => Some(Duration::from_secs(seconds)),
        Err(err) => {
            warn!(
                value = raw.as_str(),
                error = %err,
                "ignoring invalid GATEWAY_BACKEND_REQUEST_TIMEOUT_SECS"
            );
            None
        }
    }
}

fn conversation_state_store_for_config(config: &GatewayConfig) -> ConversationStateStore {
    let store = if let Some(path) = config
        .workflow
        .conversation_state
        .persistence_root
        .as_deref()
    {
        ConversationStateStore::new_with_policy(
            path,
            config.workflow.conversation_state.corruption_policy,
        )
    } else {
        let default_store = ConversationStateStore::default();
        ConversationStateStore::new_with_policy(
            default_store.root(),
            config.workflow.conversation_state.corruption_policy,
        )
    };

    if let Some(max_session_age_days) = config
        .workflow
        .conversation_state
        .retention
        .max_session_age_days
    {
        match store.cleanup_sessions_older_than_days(max_session_age_days) {
            Ok(removed) if removed > 0 => {
                info!(
                    removed_session_buckets = removed,
                    max_session_age_days, "removed expired conversation-state session buckets"
                );
            }
            Ok(_) => {}
            Err(error) => {
                warn!(
                    error = %error,
                    max_session_age_days,
                    "failed to clean up expired conversation-state session buckets"
                );
            }
        }
    }
    store
}

fn auth_path_override(state: &AppState) -> Option<&Path> {
    #[cfg(test)]
    {
        state.auth_json_path.as_deref()
    }

    #[cfg(not(test))]
    {
        let _ = state;
        None
    }
}

fn default_claude_gateway_settings_path() -> PathBuf {
    if let Ok(path) = std::env::var("CLAUDE_GATEWAY_SETTINGS_PATH") {
        return PathBuf::from(path);
    }

    if let Ok(claude_gateway_home) = std::env::var("CLAUDE_GATEWAY_HOME") {
        return PathBuf::from(claude_gateway_home).join("settings.json");
    }

    let home = std::env::var("HOME").map_or_else(|_| PathBuf::from("."), PathBuf::from);
    home.join(".claude_gateway").join("settings.json")
}

fn load_claude_gateway_settings(path: &Path) -> Result<ClaudeGatewaySettings, String> {
    let bytes = fs::read(path).map_err(|err| {
        format!(
            "failed to read Claude gateway settings at {}: {err}",
            path.display()
        )
    })?;
    serde_json::from_slice::<ClaudeGatewaySettings>(&bytes).map_err(|err| {
        format!(
            "failed to parse Claude gateway settings at {}: {err}",
            path.display()
        )
    })
}

fn model_catalog_from_settings(
    settings: &ClaudeGatewaySettings,
) -> Vec<ClaudeGatewayModelsResponseItem> {
    if !settings.models.is_empty() {
        return dedupe_models(
            settings
                .models
                .iter()
                .map(|model| ClaudeGatewayModelsResponseItem {
                    id: model.id.clone(),
                    item_type: "model",
                    name: model.name.clone(),
                    description: model.description.clone(),
                })
                .collect(),
        );
    }

    let mut models = Vec::new();
    add_model_from_env(
        &settings.env,
        "ANTHROPIC_DEFAULT_HAIKU_MODEL",
        "ANTHROPIC_DEFAULT_HAIKU_MODEL_NAME",
        "ANTHROPIC_DEFAULT_HAIKU_MODEL_DESCRIPTION",
        "GPT-5.4 Mini",
        "OpenAI small/fallback model",
        &mut models,
    );
    add_model_from_env(
        &settings.env,
        "ANTHROPIC_DEFAULT_SONNET_MODEL",
        "ANTHROPIC_DEFAULT_SONNET_MODEL_NAME",
        "ANTHROPIC_DEFAULT_SONNET_MODEL_DESCRIPTION",
        "GPT-5.4",
        "OpenAI general-purpose model",
        &mut models,
    );
    add_model_from_env(
        &settings.env,
        "ANTHROPIC_DEFAULT_OPUS_MODEL",
        "ANTHROPIC_DEFAULT_OPUS_MODEL_NAME",
        "ANTHROPIC_DEFAULT_OPUS_MODEL_DESCRIPTION",
        "GPT-5.5",
        "OpenAI reasoning model",
        &mut models,
    );
    add_model_from_env(
        &settings.env,
        "ANTHROPIC_DEFAULT_FABLE_MODEL",
        "ANTHROPIC_DEFAULT_FABLE_MODEL_NAME",
        "ANTHROPIC_DEFAULT_FABLE_MODEL_DESCRIPTION",
        "GPT-5.5 Pro",
        "OpenAI highest-capability model",
        &mut models,
    );
    add_model_from_env(
        &settings.env,
        "ANTHROPIC_CUSTOM_MODEL_OPTION",
        "ANTHROPIC_CUSTOM_MODEL_OPTION_NAME",
        "ANTHROPIC_CUSTOM_MODEL_OPTION_DESCRIPTION",
        "Custom model",
        "Custom model option",
        &mut models,
    );

    dedupe_models(models)
}

fn add_model_from_env(
    env: &HashMap<String, serde_json::Value>,
    id_key: &str,
    name_key: &str,
    description_key: &str,
    default_name: &str,
    default_description: &str,
    models: &mut Vec<ClaudeGatewayModelsResponseItem>,
) {
    let Some(id) = env.get(id_key).and_then(serde_json::Value::as_str) else {
        return;
    };

    models.push(ClaudeGatewayModelsResponseItem {
        id: id.to_string(),
        item_type: "model",
        name: env
            .get(name_key)
            .and_then(serde_json::Value::as_str)
            .map(ToString::to_string)
            .or_else(|| Some(default_name.to_string())),
        description: env
            .get(description_key)
            .and_then(serde_json::Value::as_str)
            .map(ToString::to_string)
            .or_else(|| Some(default_description.to_string())),
    });
}

fn dedupe_models(
    models: Vec<ClaudeGatewayModelsResponseItem>,
) -> Vec<ClaudeGatewayModelsResponseItem> {
    let mut seen = HashSet::new();
    models
        .into_iter()
        .filter(|model| seen.insert(model.id.clone()))
        .collect()
}

pub fn router(state: AppState) -> axum::Router {
    axum::Router::new()
        .route("/health", get(health))
        .route("/auth/status", get(auth_status))
        .route("/auth/refresh", post(auth_refresh))
        .route("/v1/models", get(v1_models_with_state))
        .route("/v1/messages", post(v1_messages))
        .fallback(fallback_404)
        .with_state(state)
}

async fn health() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok" }))
}

async fn auth_status(State(state): State<AppState>) -> impl IntoResponse {
    let status = match auth_path_override(&state) {
        Some(path) => gateway_auth_codex::load_gateway_auth_status(path),
        None => gateway_auth_codex::load_gateway_auth_status_default_path(),
    };
    auth_status_response(status)
}

fn auth_status_response(
    status: Result<
        Option<gateway_auth_codex::GatewayAuthStatus>,
        gateway_auth_codex::CodexAuthError,
    >,
) -> Json<serde_json::Value> {
    match status {
        Ok(Some(status)) => Json(serde_json::json!({
            "logged_in": status.ready_for_messages(),
            "ready_for_messages": status.ready_for_messages(),
            "ready_for_models": status.ready_for_models(),
            "account_id": status.account_id,
            "login_method": match status.login_method {
                gateway_auth_codex::GatewayLoginMethod::Chatgpt => "chatgpt",
                gateway_auth_codex::GatewayLoginMethod::ApiKey => "api_key",
            },
            "source": "gateway_auth_json",
        })),
        Ok(None) => Json(serde_json::json!({
            "logged_in": false,
            "account_id": null,
            "login_method": null,
            "source": "gateway_auth_json",
            "auth_remediation": "Please run: cld-gateway login claude",
        })),
        Err(err) => Json(serde_json::json!({
            "logged_in": false,
            "account_id": null,
            "login_method": null,
            "source": "error",
            "error_type": format!("{err}"),
            "auth_remediation": "Please run: cld-gateway login claude",
        })),
    }
}

async fn auth_refresh(State(state): State<AppState>) -> axum::response::Response {
    match state.auth.refresh_and_persist_default_path().await {
        Ok(snap) => Json(serde_json::json!({
            "ok": true,
            "account_id": snap.account_id,
            "expires_at_unix_seconds": snap.expires_at_unix_seconds,
            "source": "gateway_auth_json",
        }))
        .into_response(),
        Err(err) => auth_error(&format!("{err}")),
    }
}

async fn fallback_404() -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({
            "error": { "type": "not_found", "message": "not found" }
        })),
    )
}

async fn v1_models_with_state(State(state): State<AppState>) -> axum::response::Response {
    let settings_path = state
        .claude_gateway_settings_path
        .as_deref()
        .map_or_else(default_claude_gateway_settings_path, Path::to_path_buf);

    let settings = match load_claude_gateway_settings(&settings_path) {
        Ok(settings) => settings,
        Err(message) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": {
                        "type": "config_error",
                        "message": message
                    }
                })),
            )
                .into_response();
        }
    };

    let data = model_catalog_from_settings(&settings);

    if data.is_empty() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": {
                    "type": "config_error",
                    "message": format!(
                        "no models were found in {}",
                        settings_path.display()
                    )
                }
            })),
        )
            .into_response();
    }

    Json(serde_json::json!({ "object": "list", "data": data })).into_response()
}

#[allow(clippy::too_many_lines)]
async fn v1_messages(
    State(state): State<AppState>,
    request_id: Option<axum::extract::Extension<RequestId>>,
    req: Request,
) -> axum::response::Response {
    let claude_session_id = claude_session_id_from_headers(req.headers());
    let body = match read_request_body(req).await {
        Ok(b) => b,
        Err(resp) => return resp,
    };

    let mut req: AnthropicMessagesRequest = match deserialize_with_path(&body) {
        Ok(v) => v,
        Err(err) => return bad_request(&err),
    };

    log_ignored_stop_sequences(&req.stop_sequences, request_id.as_ref());
    log_ignored_request_controls(&req, request_id.as_ref());
    let context_management_report =
        apply_context_management(&state.gateway_config, &mut req, request_id.as_ref());
    validate_tool_results(&state, &req);

    if req.stream {
        return stream_messages(
            state,
            request_id,
            claude_session_id,
            req,
            context_management_report,
        )
        .await
        .into_response();
    }

    let resolution = resolve_and_log_model(
        &state.gateway_config,
        &req.model,
        request_id.as_ref(),
        "resolved model for /v1/messages",
    );
    let preview_tool_context = build_tool_translation_context(&state, &req);
    let translated_preview = match translate_request_with_context(&req, &preview_tool_context) {
        Ok(t) => t,
        Err(err) => return bad_request(&err),
    };
    let prepared_branch = prepare_conversation_branch(
        &state,
        claude_session_id.as_deref(),
        &req,
        translated_preview.client_metadata.as_ref(),
    );
    log_conversation_branch_resolution(request_id.as_ref(), prepared_branch.as_ref());
    let render_req = prepared_branch.as_ref().map_or_else(
        || req.clone(),
        |prepared_branch| request_with_messages(&req, prepared_branch.active_messages.clone()),
    );
    let tool_context = build_tool_translation_context(&state, &render_req);
    let mut translated = match translate_request_with_context(&render_req, &tool_context) {
        Ok(t) => t,
        Err(err) => return bad_request(&err),
    };
    attach_context_management_metadata(&mut translated, &context_management_report);

    // Check for translated command BEFORE requiring credentials.
    // This allows /status to work even without valid auth credentials.
    if let Some(cmd_name) = translated.client_metadata.as_ref().and_then(|m| {
        m.get("claude_code_translated_slash_command")
            .map(std::string::String::as_str)
    }) {
        let maybe_creds = load_codex_credentials(auth_path_override(&state)).ok();
        let config_path = gateway_core::config::default_gateway_config_path();
        let runtime = ExecutorRuntime {
            credentials: maybe_creds.clone(),
            backend_client: state.backend.clone(),
            current_model: Some(render_req.model.clone()),
            session_info: translate_executor::SessionInfo {
                // Thread/session tracking is client-side (Claude Code); the gateway
                // is a stateless proxy and does not maintain session state across requests.
                thread_id: None,
                thread_name: None,
                // Account display is derived from credentials when available.
                account_display: maybe_creds.as_ref().map(|c| c.account_id.clone()),
            },
            gateway_version: env!("CARGO_PKG_VERSION"),
            config_path: Some(config_path.display().to_string()),
            resolved_model: Some(resolution.selected_backend_model.clone()),
            current_dir: std::env::current_dir()
                .ok()
                .map(|p| p.display().to_string()),
            reasoning_effort: translated
                .client_metadata
                .as_ref()
                .and_then(|m| m.get("anthropic_effort").cloned()),
        };
        match execute_translated_command(Some(cmd_name), &runtime).await {
            Ok(Some(executor_json)) => {
                // Look up the post-result wrapper function for this command.
                let Some(post_result_fn) = translate_executor::get_post_result_function(cmd_name)
                else {
                    let error_message = format!(
                        "No post-result function registered for translated command '{cmd_name}'"
                    );
                    tracing::error!(
                        command = %cmd_name,
                        "translated command missing post-result function; returning error"
                    );
                    let error_response = serde_json::json!({
                        "type": "error",
                        "error": {
                            "type": "invalid_request_error",
                            "message": error_message
                        }
                    });
                    return (StatusCode::OK, Json(error_response)).into_response();
                };
                let packaged_body = crate::claude_code_context::get_packaged_command_body(cmd_name);
                let result_text = post_result_fn(&executor_json, packaged_body);
                // Append result as a user message to the backend input.
                translated.input.push(serde_json::json!({
                    "type": "message",
                    "role": "user",
                    "content": [{ "type": "input_text", "text": result_text }]
                }));
            }
            Ok(None) => {
                // Classified as Translate but no executor registered — this is a bug.
                let error_message =
                    format!("No executor registered for translated command '{cmd_name}'");
                tracing::error!(
                    command = %cmd_name,
                    "translated command has no executor; returning error"
                );
                let error_response = serde_json::json!({
                    "type": "error",
                    "error": {
                        "type": "invalid_request_error",
                        "message": error_message
                    }
                });
                return (StatusCode::OK, Json(error_response)).into_response();
            }
            Err(err) => {
                // Executor failure is explicit; return error instead of silently degrading.
                let error_response = serde_json::json!({
                    "type": "error",
                    "error": {
                        "type": "invalid_request_error",
                        "message": format!("Translated command '{}' failed: {}", cmd_name, err)
                    }
                });
                if let Some(axum::extract::Extension(rid)) = request_id.as_ref() {
                    tracing::error!(
                        request_id = %rid.0,
                        error = %err,
                        command = %cmd_name,
                        "translated command executor failed; returning error"
                    );
                } else {
                    tracing::error!(
                        error = %err,
                        command = %cmd_name,
                        "translated command executor failed; returning error"
                    );
                }
                return (StatusCode::OK, Json(error_response)).into_response();
            }
        }
    }

    // For non-translated commands, credentials are required.
    let creds = match load_codex_credentials(auth_path_override(&state)) {
        Ok(c) => c,
        Err(err) => return auth_error(&err),
    };
    let request_compatibility_fingerprint =
        request_compatibility_fingerprint(&state.gateway_config, &resolution, &translated);

    let selected_transport = match select_transport(
        &state.gateway_config,
        prepared_branch.as_ref(),
        &resolution.selected_backend_model,
        &request_compatibility_fingerprint,
    ) {
        Ok(selection) => selection,
        Err(err) => return service_unavailable_error(&err),
    };
    log_transport_selection(
        request_id.as_ref(),
        prepared_branch.as_ref(),
        &selected_transport,
        &resolution.selected_backend_model,
    );

    let backend_req = build_backend_request(
        &state.gateway_config,
        &resolution,
        translated,
        creds,
        selected_transport.previous_response_id.clone(),
    );
    let provider_model_fingerprint = resolution.selected_backend_model.clone();
    let (decoded, request_previous_response_id) = match run_backend_unary(
        &state,
        request_id.as_ref(),
        prepared_branch.as_ref(),
        backend_req,
    )
    .await
    {
        Ok(value) => value,
        Err(resp) => return resp,
    };
    let logged_previous_response_id = request_previous_response_id.clone();

    if let Some(prepared_branch) = prepared_branch
        .as_ref()
        .filter(|prepared_branch| prepared_branch.turn_scope == ConversationTurnScope::Main)
        && let Err(err) = state.conversation_state.commit_turn(
            &prepared_branch.claude_session_id,
            &prepared_branch.branch.branch_id,
            &CommitTurnParams {
                turn_scope: prepared_branch.turn_scope,
                turn_id: format!("turn_{}", Uuid::new_v4()),
                fingerprints: prepared_branch.fingerprints.clone(),
                provider_response_id: decoded.response_id.clone(),
                previous_response_id: request_previous_response_id,
                provider_model_fingerprint: Some(provider_model_fingerprint),
                request_compatibility_fingerprint: Some(request_compatibility_fingerprint),
                provider_output_items: decoded.output_items.clone(),
            },
        )
    {
        warn!(error = %err, "failed to commit unary conversation-state turn");
    } else if let Some(prepared_branch) = prepared_branch.as_ref()
        && let Some(response_id) = decoded.response_id.as_deref()
    {
        if let Some(request_id) = request_id_str(request_id.as_ref()) {
            info!(
                request_id = %request_id,
                claude_session_id = %prepared_branch.claude_session_id,
                branch_id = %prepared_branch.branch.branch_id,
                provider_response_id = %response_id,
                previous_response_id = ?logged_previous_response_id,
                compaction_reset_pending = false,
                "captured unary provider checkpoint response id"
            );
        } else {
            info!(
                claude_session_id = %prepared_branch.claude_session_id,
                branch_id = %prepared_branch.branch.branch_id,
                provider_response_id = %response_id,
                previous_response_id = ?logged_previous_response_id,
                compaction_reset_pending = false,
                "captured unary provider checkpoint response id"
            );
        }
    }

    let mut response = build_unary_messages_response(&state, &req, request_id.as_ref(), &decoded);
    if let Some(context_management) = context_management_report.response_value() {
        response["context_management"] = context_management;
    }

    let mut http_res = Json(response).into_response();
    http_res.extensions_mut().insert(resolution);
    http_res
}

fn build_unary_messages_response(
    state: &AppState,
    req: &AnthropicMessagesRequest,
    request_id: Option<&axum::extract::Extension<RequestId>>,
    decoded: &gateway_backend_codex::types::CodexUnaryDecoded,
) -> serde_json::Value {
    let usage = decoded.token_usage.map_or(
        serde_json::json!({
            "input_tokens": 0,
            "output_tokens": 0
        }),
        |u| {
            let mut value = serde_json::json!({
                "input_tokens": u.input_tokens,
                "output_tokens": u.output_tokens
            });
            if u.web_search_requests > 0 {
                value["server_tool_use"] = serde_json::json!({
                    "web_search_requests": u.web_search_requests,
                    "web_fetch_requests": 0
                });
            }
            value
        },
    );

    if decoded.tool_calls.is_empty() {
        return serde_json::json!({
            "id": format!("msg_{}", Uuid::new_v4()),
            "type": "message",
            "role": "assistant",
            "model": req.model,
            "content": [{ "type": "text", "text": decoded.final_text.clone() }],
            "stop_reason": "end_turn",
            "stop_sequence": null,
            "usage": usage
        });
    }

    let request_id_str = request_id.map(|axum::extract::Extension(r)| r.0.as_str());
    let mut content = Vec::new();
    if !decoded.final_text.is_empty() {
        content.push(serde_json::json!({ "type": "text", "text": decoded.final_text.clone() }));
    }
    for tool_call in &decoded.tool_calls {
        let _ = state.tool_calls.record_tool_call(
            &tool_call.call_id,
            &tool_call.name,
            tool_call.kind.as_str(),
            request_id_str,
        );
        content.push(tool_call_content_block(tool_call));
    }

    serde_json::json!({
        "id": format!("msg_{}", Uuid::new_v4()),
        "type": "message",
        "role": "assistant",
        "model": req.model,
        "content": content,
        "stop_reason": "tool_use",
        "stop_sequence": null,
        "usage": usage
    })
}

fn tool_call_content_block(tool_call: &CodexToolCall) -> serde_json::Value {
    let input_value = crate::tool_arg_policy::sanitized_tool_args_for_kind(
        &tool_call.name,
        tool_call.kind,
        &tool_call.arguments,
    )
    .map_or_else(
        |_| serde_json::json!({}),
        |(args, _edits)| serde_json::Value::Object(args),
    );
    serde_json::json!({
        "type": "tool_use",
        "id": tool_call.call_id,
        "name": tool_call.name,
        "input": input_value
    })
}

fn resolve_and_log_model(
    config: &GatewayConfig,
    model: &str,
    request_id: Option<&axum::extract::Extension<RequestId>>,
    msg: &str,
) -> ModelResolution {
    let resolution = resolve_model(config, model);
    if let Some(axum::extract::Extension(rid)) = request_id {
        info!(
            request_id = %rid.0,
            requested_model = %resolution.requested,
            selected_backend_model = %resolution.selected_backend_model,
            selection_reason = %resolution.selection_reason,
            "{msg}"
        );
    }
    resolution
}

fn load_codex_credentials(
    auth_path: Option<&Path>,
) -> Result<gateway_auth_codex::CodexCredentials, String> {
    let result = match auth_path {
        Some(path) => gateway_auth_codex::load_credentials(path),
        None => gateway_auth_codex::load_credentials_default_path(),
    };
    result.map_err(|err| format!("{err}"))
}

fn build_backend_request(
    config: &GatewayConfig,
    resolution: &ModelResolution,
    translated: crate::translate::TranslateResult,
    creds: gateway_auth_codex::CodexCredentials,
    previous_response_id: Option<String>,
) -> gateway_backend_codex::types::CodexBackendRequest {
    gateway_backend_codex::types::CodexBackendRequest {
        access_token: creds.access_token,
        account_id: creds.account_id,
        model: resolution.selected_backend_model.clone(),
        instructions: translated.instructions,
        input: translated.input,
        tools: translated.tools,
        tool_choice: translated.tool_choice,
        parallel_tool_calls: translated.parallel_tool_calls,
        text: translated.text,
        reasoning: translated.reasoning,
        previous_response_id,
        store: false,
        stream: true,
        include: translated.include,
        service_tier: service_tier_for_config(config),
        client_metadata: translated.client_metadata,
    }
}

fn request_compatibility_fingerprint(
    config: &GatewayConfig,
    resolution: &ModelResolution,
    translated: &crate::translate::TranslateResult,
) -> String {
    let payload = serde_json::json!({
        "renderer_version": "gateway_http_anthropic_transport_v1",
        "selected_backend_model": resolution.selected_backend_model,
        "instructions": translated.instructions,
        "tools": translated.tools,
        "tool_choice": translated.tool_choice,
        "parallel_tool_calls": translated.parallel_tool_calls,
        "text": translated.text,
        "reasoning": translated.reasoning,
        "include": translated.include,
        "client_metadata": translated.client_metadata,
        "service_tier": service_tier_for_config(config),
        "incremental_transport_mode": config.providers.openai.incremental_transport.mode,
    });
    hash_serde_value(&payload)
}

fn apply_context_management(
    config: &GatewayConfig,
    req: &mut AnthropicMessagesRequest,
    request_id: Option<&axum::extract::Extension<RequestId>>,
) -> ContextManagementReport {
    let report = ContextManager::new(&config.workflow.context_management)
        .apply(req.context_management.as_ref(), &mut req.messages);
    if let Some(metadata) = report.metadata_value() {
        if let Some(axum::extract::Extension(rid)) = request_id {
            info!(
                request_id = %rid.0,
                context_management = %metadata,
                "applied gateway context management"
            );
        } else {
            info!(
                context_management = %metadata,
                "applied gateway context management"
            );
        }
    }
    report
}

fn attach_context_management_metadata(
    translated: &mut crate::translate::TranslateResult,
    report: &ContextManagementReport,
) {
    let Some(metadata) = report.metadata_value() else {
        return;
    };
    let encoded = serde_json::to_string(&metadata).unwrap_or_else(|_| "{}".to_string());
    translated
        .client_metadata
        .get_or_insert_with(HashMap::new)
        .insert("gateway_context_management".to_string(), encoded);
}

async fn run_backend_unary(
    state: &AppState,
    request_id: Option<&axum::extract::Extension<RequestId>>,
    prepared_branch: Option<&PreparedConversationBranch>,
    backend_req: gateway_backend_codex::types::CodexBackendRequest,
) -> Result<
    (
        gateway_backend_codex::types::CodexUnaryDecoded,
        Option<String>,
    ),
    axum::response::Response,
> {
    let (res, effective_previous_response_id) = send_backend_stream_with_delta_fallback(
        state,
        request_id_str(request_id).map(ToString::to_string),
        prepared_branch,
        backend_req,
    )
    .await
    .map_err(backend_request_failure_to_http_response)?;

    let decoded = gateway_backend_codex::sse_unary::read_sse_to_completion(res.bytes_stream())
        .await
        .map_err(|err| {
            (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "error": { "type": "backend_error", "message": format!("{err}") }
                })),
            )
                .into_response()
        })?;

    Ok((decoded, effective_previous_response_id))
}

#[derive(Debug)]
enum BackendRequestFailure {
    Backend(BackendError),
    CheckpointInvalidation(String),
}

async fn send_backend_stream_with_delta_fallback(
    state: &AppState,
    request_id: Option<String>,
    prepared_branch: Option<&PreparedConversationBranch>,
    mut backend_req: gateway_backend_codex::types::CodexBackendRequest,
) -> Result<(reqwest::Response, Option<String>), BackendRequestFailure> {
    let attempted_previous_response_id = backend_req.previous_response_id.clone();
    match state
        .backend
        .send_streaming_with_refresh_retry(&state.auth, backend_req.clone())
        .await
    {
        Ok(response) => Ok((response, attempted_previous_response_id)),
        Err(err) if is_known_delta_rejection(&err, attempted_previous_response_id.as_deref()) => {
            let Some(prepared_branch) = prepared_branch else {
                return Err(BackendRequestFailure::Backend(err));
            };

            state
                .conversation_state
                .invalidate_openai_checkpoint(
                    &prepared_branch.claude_session_id,
                    &prepared_branch.branch.branch_id,
                )
                .map_err(|state_err| {
                    BackendRequestFailure::CheckpointInvalidation(state_err.to_string())
                })?;

            if let Some(request_id) = request_id.as_deref() {
                warn!(
                    request_id = %request_id,
                    claude_session_id = %prepared_branch.claude_session_id,
                    branch_id = %prepared_branch.branch.branch_id,
                    previous_response_id = ?attempted_previous_response_id,
                    error = %err,
                    fallback_reason = "delta_checkpoint_rejected",
                    "backend rejected incremental checkpoint; retrying once with a full request"
                );
            } else {
                warn!(
                    claude_session_id = %prepared_branch.claude_session_id,
                    branch_id = %prepared_branch.branch.branch_id,
                    previous_response_id = ?attempted_previous_response_id,
                    error = %err,
                    fallback_reason = "delta_checkpoint_rejected",
                    "backend rejected incremental checkpoint; retrying once with a full request"
                );
            }

            backend_req.previous_response_id = None;
            let full_response = state
                .backend
                .send_streaming_with_refresh_retry(&state.auth, backend_req)
                .await
                .map_err(BackendRequestFailure::Backend)?;
            Ok((full_response, None))
        }
        Err(err) => Err(BackendRequestFailure::Backend(err)),
    }
}

fn is_known_delta_rejection(
    err: &BackendError,
    attempted_previous_response_id: Option<&str>,
) -> bool {
    let Some(previous_response_id) = attempted_previous_response_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return false;
    };

    match err {
        BackendError::UnexpectedStatusWithBody { status, body }
            if matches!(*status, 400 | 404 | 409 | 410 | 422) =>
        {
            let body = body.to_ascii_lowercase();
            let previous_response_id = previous_response_id.to_ascii_lowercase();
            let mentions_checkpoint = body.contains("previous_response_id")
                || body.contains("response_id")
                || body.contains(&previous_response_id);
            let indicates_stale_chain = [
                "not found",
                "does not exist",
                "invalid",
                "unknown",
                "stale",
                "expired",
            ]
            .iter()
            .any(|needle| body.contains(needle));

            mentions_checkpoint && indicates_stale_chain
        }
        _ => false,
    }
}

fn backend_request_failure_to_http_response(
    err: BackendRequestFailure,
) -> axum::response::Response {
    match err {
        BackendRequestFailure::Backend(err) => match err {
            BackendError::AuthFailed { stage: _, message } => auth_error(&message),
            BackendError::UnexpectedStatusWithBody { status: 401, body } => auth_error(&body),
            BackendError::UnexpectedStatus(401) => auth_error("Authentication failed"),
            _ => (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "error": { "type": "backend_error", "message": err.to_string() }
                })),
            )
                .into_response(),
        },
        BackendRequestFailure::CheckpointInvalidation(message) => {
            service_unavailable_error(&message)
        }
    }
}

fn backend_request_failure_to_sse(
    err: BackendRequestFailure,
) -> Sse<futures_util::stream::BoxStream<'static, Result<Event, std::convert::Infallible>>> {
    match err {
        BackendRequestFailure::Backend(err) => match err {
            BackendError::AuthFailed { stage: _, message } => sse_auth_error(&message),
            BackendError::UnexpectedStatusWithBody { status: 401, body } => sse_auth_error(&body),
            BackendError::UnexpectedStatus(401) => sse_auth_error("Authentication failed"),
            _ => sse_error("backend_error", &err.to_string()),
        },
        BackendRequestFailure::CheckpointInvalidation(message) => {
            sse_error("service_unavailable_error", &message)
        }
    }
}

fn log_ignored_stop_sequences(
    stop_sequences: &[String],
    request_id: Option<&axum::extract::Extension<RequestId>>,
) {
    if stop_sequences.is_empty() {
        return;
    }
    let count = stop_sequences.len();
    if let Some(axum::extract::Extension(rid)) = request_id {
        tracing::warn!(
            request_id = %rid.0,
            count,
            "ignoring Anthropic stop_sequences (unsupported)"
        );
    } else {
        tracing::warn!(count, "ignoring Anthropic stop_sequences (unsupported)");
    }
}

fn log_ignored_request_controls(
    req: &AnthropicMessagesRequest,
    request_id: Option<&axum::extract::Extension<RequestId>>,
) {
    // Read-and-log any controls we parse but do not forward yet. This both documents behavior and
    // avoids silent drift from Claude Code expectations.
    let rid = request_id.map(|axum::extract::Extension(r)| r.0.clone());

    if req.max_tokens.is_some() {
        if let Some(rid) = &rid {
            tracing::warn!(request_id = %rid, "ignoring Anthropic max_tokens (not forwarded yet)");
        } else {
            tracing::warn!("ignoring Anthropic max_tokens (not forwarded yet)");
        }
    }
    if req.top_k.is_some() {
        if let Some(rid) = &rid {
            tracing::warn!(request_id = %rid, "ignoring Anthropic top_k (no backend equivalent)");
        } else {
            tracing::warn!("ignoring Anthropic top_k (no backend equivalent)");
        }
    }
    if req.metadata.is_some() {
        if let Some(rid) = &rid {
            tracing::warn!(request_id = %rid, "ignoring Anthropic metadata (not forwarded yet)");
        } else {
            tracing::warn!("ignoring Anthropic metadata (not forwarded yet)");
        }
    }
    if req.thinking.is_some() {
        if let Some(rid) = &rid {
            tracing::warn!(request_id = %rid, "ignoring Anthropic thinking (not forwarded yet)");
        } else {
            tracing::warn!("ignoring Anthropic thinking (not forwarded yet)");
        }
    }
}

fn validate_tool_results(state: &AppState, req: &AnthropicMessagesRequest) {
    for msg in &req.messages {
        let crate::types::AnthropicContent::Blocks(blocks) = &msg.content else {
            continue;
        };
        for block in blocks {
            if block.block_type != "tool_result" {
                continue;
            }
            let Some(tool_use_id) = block.tool_use_id.as_deref() else {
                continue;
            };
            let exists = state
                .tool_calls
                .tool_call_exists(tool_use_id)
                .unwrap_or(false);
            if !exists {
                // This gateway is stateless from the client's perspective and may restart between
                // tool calls. Do not reject the request; log and allow the backend to decide.
                tracing::warn!(
                    tool_use_id,
                    "tool_result references unknown tool_use_id (gateway state missing); forwarding anyway"
                );
            }
        }
    }
}

fn build_tool_translation_context(
    state: &AppState,
    req: &AnthropicMessagesRequest,
) -> ToolTranslationContext {
    let mut tool_kinds = std::collections::HashMap::new();
    for msg in &req.messages {
        let crate::types::AnthropicContent::Blocks(blocks) = &msg.content else {
            continue;
        };
        for block in blocks {
            let call_id = match block.block_type.as_str() {
                "tool_use" => block.id.as_deref(),
                "tool_result" => block.tool_use_id.as_deref(),
                _ => None,
            };
            let Some(call_id) = call_id else {
                continue;
            };
            if tool_kinds.contains_key(call_id) {
                continue;
            }
            let stored = state.tool_calls.get_tool_call(call_id).ok().flatten();
            if let Some(stored) = stored
                && let Ok(kind) = stored.tool_kind.parse::<CodexToolCallKind>()
            {
                tool_kinds.insert(call_id.to_string(), kind);
            }
        }
    }
    ToolTranslationContext::new(tool_kinds)
        .with_claude_code_config(state.gateway_config.workflow.claude_code.clone())
}

fn sse_error(
    message_type: &str,
    message: &str,
) -> Sse<futures_util::stream::BoxStream<'static, Result<Event, std::convert::Infallible>>> {
    let payload = serde_json::json!({
        "type": "error",
        "error": { "type": message_type, "message": message }
    })
    .to_string();

    let stream = futures_util::stream::iter([Ok::<Event, std::convert::Infallible>(
        Event::default().event("error").data(payload),
    )])
    .boxed();

    Sse::new(stream).keep_alive(KeepAlive::default())
}

fn sse_auth_error(
    message: &str,
) -> Sse<futures_util::stream::BoxStream<'static, Result<Event, std::convert::Infallible>>> {
    let remediation = "Please run: cld-gateway login claude";
    let payload = serde_json::json!({
        "type": "error",
        "error": {
            "type": "auth_error",
            "message": message,
            "auth_remediation": remediation
        }
    })
    .to_string();

    let stream = futures_util::stream::iter([Ok::<Event, std::convert::Infallible>(
        Event::default().event("error").data(payload),
    )])
    .boxed();

    Sse::new(stream).keep_alive(KeepAlive::default())
}

fn anthropic_stream_start_events(msg_id: &str, model: &str) -> Vec<Event> {
    vec![
        Event::default().event("message_start").data(
            serde_json::json!({
            "type": "message_start",
            "message": {
                "id": msg_id,
                "type": "message",
                "role": "assistant",
                    "content": [],
                    "model": model,
                    "stop_reason": null,
                    "stop_sequence": null,
                    "usage": {
                        "input_tokens": 0,
                        "cache_creation_input_tokens": 0,
                        "cache_read_input_tokens": 0,
                        "output_tokens": 0
                    }
                }
            })
            .to_string(),
        ),
        Event::default().event("content_block_start").data(
            serde_json::json!({
                "type": "content_block_start",
                "index": 0,
                "content_block": { "type": "text", "text": "" }
            })
            .to_string(),
        ),
    ]
}

#[allow(clippy::too_many_lines)]
async fn stream_messages(
    state: AppState,
    request_id: Option<axum::extract::Extension<RequestId>>,
    claude_session_id: Option<String>,
    req: AnthropicMessagesRequest,
    context_management_report: ContextManagementReport,
) -> Sse<futures_util::stream::BoxStream<'static, Result<Event, std::convert::Infallible>>> {
    let resolution = resolve_and_log_model(
        &state.gateway_config,
        &req.model,
        request_id.as_ref(),
        "resolved model for /v1/messages (streaming)",
    );
    let preview_tool_context = build_tool_translation_context(&state, &req);
    let translated_preview = match translate_request_with_context(&req, &preview_tool_context) {
        Ok(t) => t,
        Err(err) => return sse_error("invalid_request_error", &err),
    };
    let prepared_branch = prepare_conversation_branch(
        &state,
        claude_session_id.as_deref(),
        &req,
        translated_preview.client_metadata.as_ref(),
    );
    log_conversation_branch_resolution(request_id.as_ref(), prepared_branch.as_ref());
    let render_req = prepared_branch.as_ref().map_or_else(
        || req.clone(),
        |prepared_branch| request_with_messages(&req, prepared_branch.active_messages.clone()),
    );
    let tool_context = build_tool_translation_context(&state, &render_req);
    let mut translated = match translate_request_with_context(&render_req, &tool_context) {
        Ok(t) => t,
        Err(err) => return sse_error("invalid_request_error", &err),
    };
    attach_context_management_metadata(&mut translated, &context_management_report);

    // Check for translated command BEFORE requiring credentials.
    // This allows /status to work even without valid auth credentials.
    if let Some(cmd_name) = translated.client_metadata.as_ref().and_then(|m| {
        m.get("claude_code_translated_slash_command")
            .map(std::string::String::as_str)
    }) {
        let maybe_creds = load_codex_credentials(auth_path_override(&state)).ok();
        let config_path = gateway_core::config::default_gateway_config_path();
        let runtime = ExecutorRuntime {
            credentials: maybe_creds.clone(),
            backend_client: state.backend.clone(),
            current_model: Some(req.model.clone()),
            session_info: translate_executor::SessionInfo {
                // Thread/session tracking is client-side (Claude Code); the gateway
                // is a stateless proxy and does not maintain session state across requests.
                thread_id: None,
                thread_name: None,
                // Account display is derived from credentials when available.
                account_display: maybe_creds.as_ref().map(|c| c.account_id.clone()),
            },
            gateway_version: env!("CARGO_PKG_VERSION"),
            config_path: Some(config_path.display().to_string()),
            resolved_model: Some(resolution.selected_backend_model.clone()),
            current_dir: std::env::current_dir()
                .ok()
                .map(|p| p.display().to_string()),
            reasoning_effort: translated
                .client_metadata
                .as_ref()
                .and_then(|m| m.get("anthropic_effort").cloned()),
        };
        match execute_translated_command(Some(cmd_name), &runtime).await {
            Ok(Some(executor_json)) => {
                // Look up the post-result wrapper function for this command.
                let Some(post_result_fn) = translate_executor::get_post_result_function(cmd_name)
                else {
                    let error_message = format!(
                        "No post-result function registered for translated command '{cmd_name}'"
                    );
                    tracing::error!(
                        command = %cmd_name,
                        "translated command missing post-result function; returning error"
                    );
                    return sse_error("invalid_request_error", &error_message);
                };
                let packaged_body = crate::claude_code_context::get_packaged_command_body(cmd_name);
                let result_text = post_result_fn(&executor_json, packaged_body);
                // Append result as a user message to the backend input.
                translated.input.push(serde_json::json!({
                    "type": "message",
                    "role": "user",
                    "content": [{ "type": "input_text", "text": result_text }]
                }));
            }
            Ok(None) => {
                // Classified as Translate but no executor registered — this is a bug.
                let error_message =
                    format!("No executor registered for translated command '{cmd_name}'");
                tracing::error!(
                    command = %cmd_name,
                    "translated command has no executor; returning error"
                );
                return sse_error("invalid_request_error", &error_message);
            }
            Err(err) => {
                // Executor failure is explicit; return error instead of silently degrading.
                let error_message = format!("Translated command '{cmd_name}' failed: {err}");
                if let Some(axum::extract::Extension(rid)) = request_id.as_ref() {
                    tracing::error!(
                        request_id = %rid.0,
                        error = %err,
                        command = %cmd_name,
                        "translated command executor failed; returning error"
                    );
                } else {
                    tracing::error!(
                        error = %err,
                        command = %cmd_name,
                        "translated command executor failed; returning error"
                    );
                }
                return sse_error("invalid_request_error", &error_message);
            }
        }
    }

    // For non-translated commands, credentials are required.
    let creds = match load_codex_credentials(auth_path_override(&state)) {
        Ok(c) => c,
        Err(err) => return sse_auth_error(&err),
    };
    let request_compatibility_fingerprint =
        request_compatibility_fingerprint(&state.gateway_config, &resolution, &translated);

    let selected_transport = match select_transport(
        &state.gateway_config,
        prepared_branch.as_ref(),
        &resolution.selected_backend_model,
        &request_compatibility_fingerprint,
    ) {
        Ok(selection) => selection,
        Err(err) => return sse_error("service_unavailable_error", &err),
    };
    log_transport_selection(
        request_id.as_ref(),
        prepared_branch.as_ref(),
        &selected_transport,
        &resolution.selected_backend_model,
    );

    let request_previous_response_id = selected_transport.previous_response_id.clone();
    let request_to_backend = build_backend_request(
        &state.gateway_config,
        &resolution,
        translated,
        creds,
        request_previous_response_id.clone(),
    );

    let backend_response = match send_backend_stream_with_delta_fallback(
        &state,
        request_id_str(request_id.as_ref()).map(ToString::to_string),
        prepared_branch.as_ref(),
        request_to_backend,
    )
    .await
    {
        Ok((response, _effective_previous_response_id)) => Ok(response),
        Err(err) => return backend_request_failure_to_sse(err),
    };

    // Precompute the Anthropic message and emit the standard event flow.
    //
    // NOTE: For now we start with a single text block at index 0. When tool calls occur, we open a
    // second block at index 1 with `type=tool_use` and stream `input_json_delta` events into it.
    let msg_id = format!("msg_{}", Uuid::new_v4());
    let model = req.model.clone();

    let start_events = anthropic_stream_start_events(&msg_id, &model);

    let initial = futures_util::stream::iter(
        start_events
            .into_iter()
            .map(Ok::<Event, std::convert::Infallible>),
    );

    let rid_str = request_id
        .as_ref()
        .map(|axum::extract::Extension(r)| r.0.clone());
    let tool_calls = state.tool_calls.clone();
    let stream_commit = prepared_branch
        .as_ref()
        .filter(|prepared_branch| prepared_branch.turn_scope == ConversationTurnScope::Main)
        .map(|prepared_branch| StreamCommitContext {
            claude_session_id: prepared_branch.claude_session_id.clone(),
            branch_id: prepared_branch.branch.branch_id.clone(),
            fingerprints: prepared_branch.fingerprints.clone(),
            provider_model_fingerprint: resolution.selected_backend_model.clone(),
            request_compatibility_fingerprint: request_compatibility_fingerprint.clone(),
            previous_response_id: request_previous_response_id.clone(),
            request_id: request_id_str(request_id.as_ref()).map(ToString::to_string),
        });

    let tail: futures_util::stream::BoxStream<'static, Result<Event, std::convert::Infallible>> =
        match backend_response {
            Ok(res) => backend_sse_to_anthropic_events(
                res,
                state.conversation_state.clone(),
                tool_calls,
                rid_str,
                context_management_report.response_value(),
                stream_commit,
            ),
            Err(err) => {
                match err {
                    BackendError::AuthFailed { stage: _, message } => {
                        return sse_auth_error(&message);
                    }
                    BackendError::UnexpectedStatusWithBody { status: 401, body } => {
                        return sse_auth_error(&body);
                    }
                    BackendError::UnexpectedStatus(401) => {
                        return sse_auth_error("Authentication failed");
                    }
                    _ => {}
                }
                let payload = serde_json::json!({
                    "type": "error",
                    "error": { "type": "upstream_error", "message": format!("{err}") }
                })
                .to_string();
                Box::pin(futures_util::stream::iter([Ok::<
                    Event,
                    std::convert::Infallible,
                >(
                    Event::default().event("error").data(payload),
                )]))
            }
        };

    let stream = initial.chain(tail).boxed();
    Sse::new(stream).keep_alive(KeepAlive::default())
}

#[derive(Debug, Clone)]
struct PreparedConversationBranch {
    claude_session_id: String,
    branch: BranchMetadata,
    selection_action: BranchSelectionAction,
    turn_scope: ConversationTurnScope,
    fingerprints: BranchFingerprintSet,
    active_messages: Vec<crate::types::AnthropicMessage>,
}

#[derive(Debug, Clone)]
struct StreamCommitContext {
    claude_session_id: String,
    branch_id: String,
    fingerprints: BranchFingerprintSet,
    provider_model_fingerprint: String,
    request_compatibility_fingerprint: String,
    previous_response_id: Option<String>,
    request_id: Option<String>,
}

fn maybe_commit_stream_completion(
    conversation_state: &ConversationStateStore,
    stream_commit: Option<&StreamCommitContext>,
    event_name: &str,
    data: &str,
) {
    if event_name != "response.completed" {
        return;
    }
    let Some(commit) = stream_commit else {
        return;
    };

    let response_id =
        gateway_backend_codex::sse_unary::extract_response_id_from_completed_event(data);
    if let Err(err) = conversation_state.commit_turn(
        &commit.claude_session_id,
        &commit.branch_id,
        &CommitTurnParams {
            turn_scope: ConversationTurnScope::Main,
            turn_id: format!("turn_{}", Uuid::new_v4()),
            fingerprints: commit.fingerprints.clone(),
            provider_response_id: response_id.clone(),
            previous_response_id: commit.previous_response_id.clone(),
            provider_model_fingerprint: Some(commit.provider_model_fingerprint.clone()),
            request_compatibility_fingerprint: Some(
                commit.request_compatibility_fingerprint.clone(),
            ),
            provider_output_items: extract_completed_output_items(data),
        },
    ) {
        warn!(error = %err, "failed to commit streaming conversation-state turn");
        return;
    }

    if let Some(response_id) = response_id.as_deref() {
        if let Some(request_id) = commit.request_id.as_deref() {
            info!(
                request_id = %request_id,
                claude_session_id = %commit.claude_session_id,
                branch_id = %commit.branch_id,
                provider_response_id = %response_id,
                previous_response_id = ?commit.previous_response_id,
                compaction_reset_pending = false,
                "captured streaming provider checkpoint response id"
            );
        } else {
            info!(
                claude_session_id = %commit.claude_session_id,
                branch_id = %commit.branch_id,
                provider_response_id = %response_id,
                previous_response_id = ?commit.previous_response_id,
                compaction_reset_pending = false,
                "captured streaming provider checkpoint response id"
            );
        }
    }
}

fn extract_completed_output_items(data: &str) -> Vec<serde_json::Value> {
    serde_json::from_str::<serde_json::Value>(data)
        .ok()
        .and_then(|value| {
            value
                .get("response")
                .and_then(|response| response.get("output"))
                .and_then(serde_json::Value::as_array)
                .cloned()
        })
        .unwrap_or_default()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransportMode {
    Full,
    Incremental,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SelectedTransport {
    mode: TransportMode,
    previous_response_id: Option<String>,
    reason: &'static str,
}

fn select_transport(
    config: &GatewayConfig,
    prepared_branch: Option<&PreparedConversationBranch>,
    provider_model_fingerprint: &str,
    request_compatibility_fingerprint: &str,
) -> Result<SelectedTransport, String> {
    let mode = config.providers.openai.incremental_transport.mode;

    let Some(prepared_branch) = prepared_branch else {
        return match mode {
            OpenAiIncrementalTransportMode::RequireDelta => Err(
                "incremental transport is required, but no conversation-state branch is available for this request".to_string(),
            ),
            OpenAiIncrementalTransportMode::AlwaysFull | OpenAiIncrementalTransportMode::Auto => {
                Ok(SelectedTransport {
                    mode: TransportMode::Full,
                    previous_response_id: None,
                    reason: "no_branch_available",
                })
            }
        };
    };

    if mode == OpenAiIncrementalTransportMode::AlwaysFull {
        return Ok(SelectedTransport {
            mode: TransportMode::Full,
            previous_response_id: None,
            reason: "always_full_mode",
        });
    }

    if prepared_branch.turn_scope != ConversationTurnScope::Main {
        return match mode {
            OpenAiIncrementalTransportMode::RequireDelta => Err(
                "incremental transport is required, but side-turn requests are not eligible for previous_response_id reuse".to_string(),
            ),
            OpenAiIncrementalTransportMode::AlwaysFull | OpenAiIncrementalTransportMode::Auto => {
                Ok(SelectedTransport {
                    mode: TransportMode::Full,
                    previous_response_id: None,
                    reason: "side_turn",
                })
            }
        };
    }

    if prepared_branch.branch.compaction_reset_pending {
        return match mode {
            OpenAiIncrementalTransportMode::RequireDelta => Err(
                "incremental transport is required, but this branch is waiting for its post-compaction full reset".to_string(),
            ),
            OpenAiIncrementalTransportMode::AlwaysFull | OpenAiIncrementalTransportMode::Auto => {
                Ok(SelectedTransport {
                    mode: TransportMode::Full,
                    previous_response_id: None,
                    reason: "compaction_reset_pending",
                })
            }
        };
    }

    let Some(checkpoint) = prepared_branch.branch.openai_checkpoint.as_ref() else {
        return match mode {
            OpenAiIncrementalTransportMode::RequireDelta => Err(
                "incremental transport is required, but this branch has no stored OpenAI checkpoint".to_string(),
            ),
            OpenAiIncrementalTransportMode::AlwaysFull | OpenAiIncrementalTransportMode::Auto => {
                Ok(SelectedTransport {
                    mode: TransportMode::Full,
                    previous_response_id: None,
                    reason: "missing_checkpoint",
                })
            }
        };
    };

    if checkpoint.provider_model_fingerprint != provider_model_fingerprint {
        return match mode {
            OpenAiIncrementalTransportMode::RequireDelta => Err(format!(
                "incremental transport is required, but branch checkpoint model '{}' does not match current model '{}'",
                checkpoint.provider_model_fingerprint, provider_model_fingerprint
            )),
            OpenAiIncrementalTransportMode::AlwaysFull | OpenAiIncrementalTransportMode::Auto => {
                Ok(SelectedTransport {
                    mode: TransportMode::Full,
                    previous_response_id: None,
                    reason: "provider_model_mismatch",
                })
            }
        };
    }

    if checkpoint.request_compatibility_fingerprint.as_deref()
        != Some(request_compatibility_fingerprint)
    {
        return match mode {
            OpenAiIncrementalTransportMode::RequireDelta => Err(
                "incremental transport is required, but the current request's non-input compatibility fingerprint does not match the stored branch checkpoint".to_string(),
            ),
            OpenAiIncrementalTransportMode::AlwaysFull | OpenAiIncrementalTransportMode::Auto => {
                Ok(SelectedTransport {
                    mode: TransportMode::Full,
                    previous_response_id: None,
                    reason: "request_compatibility_mismatch",
                })
            }
        };
    }

    Ok(SelectedTransport {
        mode: TransportMode::Incremental,
        previous_response_id: Some(checkpoint.response_id.clone()),
        reason: "branch_checkpoint_reuse",
    })
}

fn claude_session_id_from_headers(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get("x-claude-code-session-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn prepare_conversation_branch(
    state: &AppState,
    claude_session_id: Option<&str>,
    req: &AnthropicMessagesRequest,
    client_metadata: Option<&HashMap<String, String>>,
) -> Option<PreparedConversationBranch> {
    if !state.gateway_config.workflow.conversation_state.enabled {
        return None;
    }
    let claude_session_id = claude_session_id?;
    if client_metadata
        .is_some_and(|metadata| metadata.contains_key("claude_code_translated_slash_command"))
    {
        return None;
    }

    let normalized = normalize_claude_code_context(
        &req.system,
        &req.messages,
        &state.gateway_config.workflow.claude_code,
    );
    if normalized
        .client_metadata
        .get("gateway_conversation_inclusion")
        .is_some_and(|mode| mode == "local_only")
    {
        return None;
    }
    let compaction_command_seen = has_local_only_command(&normalized.client_metadata, "/compact");
    let turn_scope = match normalized
        .client_metadata
        .get("gateway_conversation_inclusion")
    {
        Some(mode) if mode == "read_only" => ConversationTurnScope::Side,
        _ => ConversationTurnScope::Main,
    };
    let fingerprints =
        branch_fingerprints_from_messages(&normalized.messages, compaction_command_seen);
    let active_canonical_messages = serde_json::to_value(&normalized.messages).ok();
    let selection = state
        .conversation_state
        .select_or_create_branch(
            claude_session_id,
            &BranchSelectionInput {
                active_canonical_messages: active_canonical_messages.clone(),
                fingerprints: fingerprints.clone(),
                turn_scope,
            },
        )
        .ok()?;
    let mut branch = selection.branch;
    let previous_compaction_summary_hash = selection
        .matched_existing_branch
        .as_ref()
        .and_then(|existing| existing.fingerprints.compaction_summary_hash.clone());
    if compaction_command_seen
        && previous_compaction_summary_hash != fingerprints.compaction_summary_hash
    {
        branch = state
            .conversation_state
            .apply_compaction(
                claude_session_id,
                &branch.branch_id,
                fingerprints.compaction_summary_hash.as_deref(),
                &fingerprints,
            )
            .ok()?;
    }

    let active_messages = if turn_scope == ConversationTurnScope::Main {
        branch = state
            .conversation_state
            .reconcile_branch_snapshot(
                claude_session_id,
                &branch.branch_id,
                &ReconcileSnapshotParams {
                    messages: active_canonical_messages?,
                    fingerprints: fingerprints.clone(),
                },
            )
            .ok()?;

        deserialize_active_messages(branch.active_canonical_messages.as_ref())?
    } else {
        normalized.messages.clone()
    };

    Some(PreparedConversationBranch {
        claude_session_id: claude_session_id.to_string(),
        branch,
        selection_action: selection.action,
        turn_scope,
        fingerprints,
        active_messages,
    })
}

fn request_id_str(request_id: Option<&axum::extract::Extension<RequestId>>) -> Option<&str> {
    request_id.map(|axum::extract::Extension(rid)| rid.0.as_str())
}

fn log_conversation_branch_resolution(
    request_id: Option<&axum::extract::Extension<RequestId>>,
    prepared_branch: Option<&PreparedConversationBranch>,
) {
    let Some(prepared_branch) = prepared_branch else {
        if let Some(request_id) = request_id_str(request_id) {
            info!(
                request_id = %request_id,
                branch_resolution = "skipped",
                "conversation-state branch resolution produced no branch context"
            );
        } else {
            info!(
                branch_resolution = "skipped",
                "conversation-state branch resolution produced no branch context"
            );
        }
        return;
    };

    if let Some(request_id) = request_id_str(request_id) {
        info!(
            request_id = %request_id,
            claude_session_id = %prepared_branch.claude_session_id,
            branch_id = %prepared_branch.branch.branch_id,
            branch_action = ?prepared_branch.selection_action,
            turn_scope = ?prepared_branch.turn_scope,
            compaction_reset_pending = prepared_branch.branch.compaction_reset_pending,
            "selected conversation-state branch"
        );
    } else {
        info!(
            claude_session_id = %prepared_branch.claude_session_id,
            branch_id = %prepared_branch.branch.branch_id,
            branch_action = ?prepared_branch.selection_action,
            turn_scope = ?prepared_branch.turn_scope,
            compaction_reset_pending = prepared_branch.branch.compaction_reset_pending,
            "selected conversation-state branch"
        );
    }
}

fn log_transport_selection(
    request_id: Option<&axum::extract::Extension<RequestId>>,
    prepared_branch: Option<&PreparedConversationBranch>,
    selected_transport: &SelectedTransport,
    provider_model_fingerprint: &str,
) {
    let transport_mode = match selected_transport.mode {
        TransportMode::Full => "full",
        TransportMode::Incremental => "incremental",
    };

    if let Some(prepared_branch) = prepared_branch {
        if let Some(request_id) = request_id_str(request_id) {
            info!(
                request_id = %request_id,
                claude_session_id = %prepared_branch.claude_session_id,
                branch_id = %prepared_branch.branch.branch_id,
                transport_mode,
                transport_reason = selected_transport.reason,
                previous_response_id = ?selected_transport.previous_response_id,
                provider_model_fingerprint,
                compaction_reset_pending = prepared_branch.branch.compaction_reset_pending,
                "selected conversation-state transport mode"
            );
        } else {
            info!(
                claude_session_id = %prepared_branch.claude_session_id,
                branch_id = %prepared_branch.branch.branch_id,
                transport_mode,
                transport_reason = selected_transport.reason,
                previous_response_id = ?selected_transport.previous_response_id,
                provider_model_fingerprint,
                compaction_reset_pending = prepared_branch.branch.compaction_reset_pending,
                "selected conversation-state transport mode"
            );
        }
    } else if let Some(request_id) = request_id_str(request_id) {
        info!(
            request_id = %request_id,
            transport_mode,
            transport_reason = selected_transport.reason,
            previous_response_id = ?selected_transport.previous_response_id,
            provider_model_fingerprint,
            "selected conversation-state transport mode without branch context"
        );
    } else {
        info!(
            transport_mode,
            transport_reason = selected_transport.reason,
            previous_response_id = ?selected_transport.previous_response_id,
            provider_model_fingerprint,
            "selected conversation-state transport mode without branch context"
        );
    }
}

fn branch_fingerprints_from_messages(
    messages: &[crate::types::AnthropicMessage],
    compaction_command_seen: bool,
) -> BranchFingerprintSet {
    let mut text_messages = Vec::new();
    let mut last_user_text = None;

    for message in messages {
        let text = anthropic_message_text(message);
        if text.trim().is_empty() {
            continue;
        }
        if message.role == "user" {
            last_user_text = Some(text.clone());
        }
        text_messages.push(format!("{}:{}", message.role, text.trim()));
    }

    let recent_tail = text_messages
        .iter()
        .rev()
        .take(4)
        .cloned()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    let full_transcript = text_messages.join("\n");
    let branch_state_hash = (!full_transcript.is_empty()).then(|| sha256_hex(&full_transcript));

    BranchFingerprintSet {
        recent_message_tail_hash: (!recent_tail.is_empty()).then(|| sha256_hex(&recent_tail)),
        last_user_message_hash: last_user_text
            .as_deref()
            .filter(|text| !text.trim().is_empty())
            .map(sha256_hex),
        compaction_summary_hash: compaction_command_seen
            .then(|| branch_state_hash.clone())
            .flatten(),
        branch_state_hash,
    }
}

fn has_local_only_command(client_metadata: &HashMap<String, String>, command: &str) -> bool {
    client_metadata
        .get("gateway_local_only_commands")
        .is_some_and(|commands| {
            commands
                .split(',')
                .map(str::trim)
                .any(|candidate| candidate == command)
        })
}

fn anthropic_message_text(message: &crate::types::AnthropicMessage) -> String {
    match &message.content {
        crate::types::AnthropicContent::Text(text) => text.clone(),
        crate::types::AnthropicContent::Blocks(blocks) => blocks
            .iter()
            .filter(|block| block.block_type == "text")
            .filter_map(|block| block.text.as_deref())
            .collect::<Vec<_>>()
            .join("\n\n"),
    }
}

fn deserialize_active_messages(
    snapshot: Option<&serde_json::Value>,
) -> Option<Vec<crate::types::AnthropicMessage>> {
    snapshot
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
}

fn request_with_messages(
    req: &AnthropicMessagesRequest,
    messages: Vec<crate::types::AnthropicMessage>,
) -> AnthropicMessagesRequest {
    AnthropicMessagesRequest {
        model: req.model.clone(),
        messages,
        system: req.system.clone(),
        stream: req.stream,
        stop_sequences: req.stop_sequences.clone(),
        max_tokens: req.max_tokens,
        temperature: req.temperature,
        top_p: req.top_p,
        top_k: req.top_k,
        metadata: req.metadata.clone(),
        tools: req.tools.clone(),
        tool_choice: req.tool_choice.clone(),
        thinking: req.thinking.clone(),
        context_management: req.context_management.clone(),
        output_config: req.output_config.clone(),
    }
}

fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn hash_serde_value(value: &serde_json::Value) -> String {
    let encoded = serde_json::to_string(value).unwrap_or_else(|_| "null".to_string());
    sha256_hex(&encoded)
}

fn backend_sse_to_anthropic_events(
    res: reqwest::Response,
    conversation_state: ConversationStateStore,
    tool_calls: ToolCallStore,
    request_id: Option<String>,
    context_management: Option<serde_json::Value>,
    stream_commit: Option<StreamCommitContext>,
) -> futures_util::stream::BoxStream<'static, Result<Event, std::convert::Infallible>> {
    let backend_events = res.bytes_stream().eventsource();
    let state = Arc::new(Mutex::new(
        crate::sse_bridge::StreamState::new_with_text_block0_started()
            .with_context_management(context_management),
    ));
    let tool_calls = Arc::new(tool_calls);
    let request_id = Arc::new(request_id);
    let conversation_state = Arc::new(conversation_state);
    let stream_commit = Arc::new(stream_commit);

    backend_events
        .filter_map({
            let state = Arc::clone(&state);
            let tool_calls = Arc::clone(&tool_calls);
            let request_id = Arc::clone(&request_id);
            let conversation_state = Arc::clone(&conversation_state);
            let stream_commit = Arc::clone(&stream_commit);
            move |item| {
                let state = Arc::clone(&state);
                let tool_calls = Arc::clone(&tool_calls);
                let request_id = Arc::clone(&request_id);
                let conversation_state = Arc::clone(&conversation_state);
                let stream_commit = Arc::clone(&stream_commit);
                async move {
                    let mut st = state.lock().unwrap();
                    if st.completed {
                        return None;
                    }

                    let evt = match item {
                        Ok(e) => e,
                        Err(e) => {
                            st.completed = true;
                            let message = format!(
                                "event stream error: {}",
                                gateway_backend_codex::sse_unary::format_event_stream_error(&e)
                            );
                            let payload = serde_json::json!({
                                "type": "error",
                                "error": { "type": "upstream_error", "message": message }
                            })
                            .to_string();
                            return Some(vec![Event::default().event("error").data(payload)]);
                        }
                    };

                    let data = evt.data.trim();
                    if data.is_empty() || data == "[DONE]" {
                        return None;
                    }

                    let mapped = crate::sse_bridge::map_backend_event(
                        &mut st,
                        evt.event.as_str(),
                        data,
                        gateway_backend_codex::output_text::extract_text_from_data,
                        &tool_calls,
                        request_id.as_ref().as_deref(),
                    );
                    drop(st);

                    maybe_commit_stream_completion(
                        &conversation_state,
                        stream_commit.as_ref().as_ref(),
                        evt.event.as_str(),
                        data,
                    );

                    mapped
                }
            }
        })
        .flat_map(|events| {
            futures_util::stream::iter(events.into_iter().map(Ok::<_, std::convert::Infallible>))
        })
        .boxed()
}

fn deserialize_with_path<T>(body: &[u8]) -> Result<T, String>
where
    T: serde::de::DeserializeOwned,
{
    let mut de = serde_json::Deserializer::from_slice(body);
    serde_path_to_error::deserialize(&mut de).map_err(|err| err.to_string())
}

async fn read_request_body(req: Request) -> Result<Bytes, axum::response::Response> {
    // NOTE: The 5MB limit is for *logging* only. `/v1/messages` should not reject requests solely due
    // to payload size. We still need to buffer the body to parse it and to extract prompt text.
    const BODY_LIMIT_BYTES: usize = 50 * 1024 * 1024;

    let (_parts, body) = req.into_parts();
    let Ok(body) = to_bytes(body, BODY_LIMIT_BYTES).await else {
        return Err((
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(serde_json::json!({
                "error": { "type": "invalid_request_error", "message": format!("request body exceeded {BODY_LIMIT_BYTES} bytes limit") }
            })),
        )
            .into_response());
    };

    Ok(body)
}

fn bad_request(message: &str) -> axum::response::Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({
            "error": { "type": "invalid_request_error", "message": message }
        })),
    )
        .into_response()
}

fn service_unavailable_error(message: &str) -> axum::response::Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({
            "error": { "type": "service_unavailable_error", "message": message }
        })),
    )
        .into_response()
}

fn auth_error(message: &str) -> axum::response::Response {
    let remediation = "Please run: cld-gateway login claude";
    (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({
            "error": {
                "type": "auth_error",
                "message": message,
                "auth_remediation": remediation
            }
        })),
    )
        .into_response()
}

// NOTE: Request translation lives in `translate.rs`. Keep handler code free of ad-hoc extraction
// logic so we can maintain full-history fidelity.

#[cfg(test)]
mod messages_tests {
    use super::branch_fingerprints_from_messages;
    use super::build_tool_translation_context;
    use super::request_compatibility_fingerprint;
    use super::tool_call_content_block;
    use super::translate::{
        ToolTranslationContext, TranslateResult, translate_request_with_context,
    };
    use super::types::{
        AnthropicContent, AnthropicContentBlock, AnthropicMessage, AnthropicMessagesRequest,
    };
    use axum::body::Body;
    use axum::body::to_bytes;
    use axum::http::{Request, StatusCode};
    use gateway_backend_codex::types::{CodexToolCall, CodexToolCallKind};
    use gateway_core::DEFAULT_BACKEND_MODEL;
    use gateway_core::config::resolve_model;
    use gateway_core::config::{GatewayConfig, OpenAiIncrementalTransportMode};
    use gateway_state::{
        BranchCreateParams, BranchMetadata, CommitTurnParams, ConversationStateStore,
        ConversationTurnScope, ToolCallStore,
    };
    use tower::ServiceExt as _;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Match, Mock, MockServer, Request as WiremockRequest, ResponseTemplate};

    fn fixture(path: &str) -> String {
        let full = format!("{}/tests/fixtures/{}", env!("CARGO_MANIFEST_DIR"), path);
        std::fs::read_to_string(full)
            .expect("read fixture")
            .replace("__DEFAULT_BACKEND_MODEL__", DEFAULT_BACKEND_MODEL)
    }

    fn translate_request(req: &AnthropicMessagesRequest) -> Result<TranslateResult, String> {
        translate_request_with_context(req, &ToolTranslationContext::default())
    }

    fn parse_sse_frames(body: &str) -> Vec<(String, serde_json::Value)> {
        body.split("\n\n")
            .filter_map(|frame| {
                let frame = frame.trim();
                if frame.is_empty() {
                    return None;
                }
                let mut event: Option<String> = None;
                let mut data_lines: Vec<&str> = Vec::new();
                for line in frame.lines() {
                    if let Some(rest) = line.strip_prefix("event:") {
                        event = Some(rest.trim().to_string());
                    } else if let Some(rest) = line.strip_prefix("data:") {
                        data_lines.push(rest.trim());
                    }
                }
                let event = event?;
                let data_str = data_lines.join("\n");
                let data: serde_json::Value =
                    serde_json::from_str(&data_str).expect("event data json");
                Some((event, data))
            })
            .collect()
    }

    fn normalize_msg_id(
        mut events: Vec<(String, serde_json::Value)>,
    ) -> Vec<(String, serde_json::Value)> {
        for (ev, data) in &mut events {
            if ev == "message_start"
                && let Some(msg) = data.get_mut("message")
                && let Some(obj) = msg.as_object_mut()
            {
                obj.insert(
                    "id".to_string(),
                    serde_json::Value::String("msg_TEST".to_string()),
                );
            }
        }
        events
    }

    fn parse_expected_jsonl(path: &str) -> Vec<(String, serde_json::Value)> {
        let text = fixture(path);
        text.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|line| {
                let v: serde_json::Value = serde_json::from_str(line).expect("jsonl line");
                let event = v
                    .get("event")
                    .and_then(|v| v.as_str())
                    .expect("event")
                    .to_string();
                let data = v.get("data").cloned().expect("data");
                (event, data)
            })
            .collect()
    }

    #[test]
    fn translation_accepts_string_and_blocks_text() {
        let req = AnthropicMessagesRequest {
            model: DEFAULT_BACKEND_MODEL.to_string(),
            messages: vec![
                AnthropicMessage {
                    role: "system".to_string(),
                    content: AnthropicContent::Text("ignore".to_string()),
                },
                AnthropicMessage {
                    role: "user".to_string(),
                    content: AnthropicContent::Text("hello".to_string()),
                },
                AnthropicMessage {
                    role: "user".to_string(),
                    content: AnthropicContent::Blocks(vec![AnthropicContentBlock {
                        block_type: "text".to_string(),
                        text: Some("world".to_string()),
                        id: None,
                        name: None,
                        input: None,
                        tool_use_id: None,
                        content: None,
                        is_error: None,
                        source: None,
                        extra: std::collections::BTreeMap::new(),
                    }]),
                },
            ],
            system: Vec::new(),
            stream: false,
            stop_sequences: Vec::new(),
            max_tokens: None,
            temperature: None,
            top_p: None,
            top_k: None,
            metadata: None,
            tools: Vec::new(),
            tool_choice: None,
            thinking: None,
            context_management: None,
            output_config: None,
        };

        let translated = translate_request(&req).unwrap();
        assert!(!translated.input.is_empty());
    }

    #[test]
    fn unary_tool_call_content_block_supports_custom_input() {
        let block = tool_call_content_block(&CodexToolCall {
            call_id: "call_custom".to_string(),
            name: "apply_patch".to_string(),
            arguments: serde_json::json!({"input":"*** Begin Patch\n*** End Patch\n"}).to_string(),
            kind: CodexToolCallKind::Custom,
        });

        assert_eq!(block.get("type").and_then(|v| v.as_str()), Some("tool_use"));
        assert_eq!(
            block
                .get("input")
                .and_then(|v| v.get("input"))
                .and_then(|v| v.as_str()),
            Some("*** Begin Patch\n*** End Patch\n")
        );
    }

    #[test]
    fn unary_tool_call_content_block_supports_tool_search_input() {
        let block = tool_call_content_block(&CodexToolCall {
            call_id: "call_search".to_string(),
            name: "tool_search".to_string(),
            arguments: serde_json::json!({"query":"Read"}).to_string(),
            kind: CodexToolCallKind::ToolSearch,
        });

        assert_eq!(
            block.get("name").and_then(|v| v.as_str()),
            Some("tool_search")
        );
        assert_eq!(
            block
                .get("input")
                .and_then(|v| v.get("query"))
                .and_then(|v| v.as_str()),
            Some("Read")
        );
    }

    #[test]
    fn unary_tool_call_content_block_supports_local_shell_input() {
        let block = tool_call_content_block(&CodexToolCall {
            call_id: "call_shell".to_string(),
            name: "local_shell".to_string(),
            arguments: serde_json::json!({
                "status": "completed",
                "action": { "type": "exec", "command": ["echo", "hi"] }
            })
            .to_string(),
            kind: CodexToolCallKind::LocalShell,
        });

        assert_eq!(
            block
                .get("input")
                .and_then(|v| v.get("status"))
                .and_then(|v| v.as_str()),
            Some("completed")
        );
        assert_eq!(
            block
                .get("input")
                .and_then(|v| v.get("action"))
                .and_then(|v| v.get("command"))
                .and_then(|v| v.as_array())
                .and_then(|v| v.get(1))
                .and_then(|v| v.as_str()),
            Some("hi")
        );
    }

    #[test]
    fn unary_tool_call_content_block_sanitizes_read_pages() {
        let block = tool_call_content_block(&CodexToolCall {
            call_id: "call_read".to_string(),
            name: "Read".to_string(),
            arguments: serde_json::json!({
                "file_path": "/tmp/a.txt",
                "pages": ""
            })
            .to_string(),
            kind: CodexToolCallKind::Function,
        });

        let input = block
            .get("input")
            .and_then(|value| value.as_object())
            .expect("input object");
        assert_eq!(
            input.get("file_path").and_then(|value| value.as_str()),
            Some("/tmp/a.txt")
        );
        assert!(input.get("pages").is_none());
    }

    #[test]
    fn unary_tool_call_content_block_removes_agent_isolation() {
        let block = tool_call_content_block(&CodexToolCall {
            call_id: "call_agent".to_string(),
            name: "Agent".to_string(),
            arguments: serde_json::json!({
                "description": "Research files",
                "prompt": "Inspect relevant files",
                "isolation": "worktree",
                "subagent_type": "Explore"
            })
            .to_string(),
            kind: CodexToolCallKind::Function,
        });

        let input = block
            .get("input")
            .and_then(|value| value.as_object())
            .expect("input object");
        assert_eq!(
            input.get("description").and_then(|value| value.as_str()),
            Some("Research files")
        );
        assert!(input.get("isolation").is_none());
    }

    #[test]
    fn translation_context_uses_stored_tool_kind_without_translator_io() {
        let tool_calls_path = std::env::temp_dir().join(format!(
            "gateway_tool_context_{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let tool_calls = ToolCallStore::new(&tool_calls_path);
        tool_calls
            .record_tool_call(
                "call_custom",
                "apply_patch",
                "custom_tool_call",
                Some("rid_1"),
            )
            .expect("record");
        let state = super::AppState {
            tool_calls,
            ..super::AppState::default()
        };

        let mut req = AnthropicMessagesRequest {
            model: DEFAULT_BACKEND_MODEL.to_string(),
            messages: Vec::new(),
            system: Vec::new(),
            stream: false,
            stop_sequences: Vec::new(),
            max_tokens: None,
            temperature: None,
            top_p: None,
            top_k: None,
            metadata: None,
            tools: Vec::new(),
            tool_choice: None,
            thinking: None,
            context_management: None,
            output_config: None,
        };
        req.messages.push(AnthropicMessage {
            role: "user".to_string(),
            content: AnthropicContent::Blocks(vec![AnthropicContentBlock {
                block_type: "tool_result".to_string(),
                text: None,
                id: None,
                name: None,
                input: None,
                tool_use_id: Some("call_custom".to_string()),
                content: Some(serde_json::json!("ok")),
                is_error: Some(false),
                source: None,
                extra: std::collections::BTreeMap::new(),
            }]),
        });

        let context = build_tool_translation_context(&state, &req);
        let translated = translate_request_with_context(&req, &context).expect("translate");
        assert_eq!(
            translated
                .input
                .first()
                .and_then(|v| v.get("type"))
                .and_then(|v| v.as_str()),
            Some("custom_tool_call_output")
        );
    }

    #[tokio::test]
    async fn v1_messages_supports_stream_true() {
        if !wiremock_enabled() {
            return;
        }

        let auth_path = write_temp_auth_json();
        let mock = MockServer::start().await;
        let backend = format!("{}\n\n", fixture("streaming/backend_stream_text_only.sse"));
        Mock::given(method("POST"))
            .and(path("/backend-api/codex/responses"))
            .and(header("authorization", "Bearer tok_test"))
            .and(header("chatgpt-account-id", "acct_test"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(backend, "text/event-stream"))
            .mount(&mock)
            .await;

        let base_url = url::Url::parse(&mock.uri()).expect("mock url");
        let state = super::AppState {
            backend: gateway_backend_codex::client::CodexBackendClient::default()
                .with_base_url(&base_url),
            auth_json_path: Some(auth_path),
            ..super::AppState::default()
        };

        let app = super::router(state);
        let req_body = serde_json::json!({
            "model": DEFAULT_BACKEND_MODEL,
            "stream": true,
            "messages": [{ "role": "user", "content": "hi" }]
        });

        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert!(res.status().is_success());
        let content_type = res
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(content_type.contains("text/event-stream"));

        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let text = std::str::from_utf8(&body).unwrap();
        let got = normalize_msg_id(parse_sse_frames(text));
        let expected = parse_expected_jsonl("streaming/expected_anthropic_text_only.jsonl");
        assert_eq!(got, expected);
    }

    fn write_temp_auth_json() -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("gateway_auth_{}.json", uuid::Uuid::new_v4()));
        let value = serde_json::json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "access_token": "tok_test",
                "account_id": "acct_test"
            }
        });
        std::fs::write(&path, value.to_string()).expect("write auth fixture");
        path
    }

    fn wiremock_enabled() -> bool {
        std::env::var("RUN_WIREMOCK").ok().as_deref() == Some("1")
    }

    #[derive(Debug)]
    struct PreviousResponseIdMatcher(Option<String>);

    impl PreviousResponseIdMatcher {
        fn some(value: &str) -> Self {
            Self(Some(value.to_string()))
        }

        fn none() -> Self {
            Self(None)
        }
    }

    impl Match for PreviousResponseIdMatcher {
        fn matches(&self, request: &WiremockRequest) -> bool {
            let Ok(body) = serde_json::from_slice::<serde_json::Value>(&request.body) else {
                return false;
            };
            let seen = body
                .get("previous_response_id")
                .and_then(serde_json::Value::as_str)
                .map(ToString::to_string);
            seen == self.0
        }
    }

    #[derive(Debug)]
    struct FunctionToolResultMatcher {
        call_id: String,
        output: String,
    }

    impl FunctionToolResultMatcher {
        fn new(call_id: &str, output: &str) -> Self {
            Self {
                call_id: call_id.to_string(),
                output: output.to_string(),
            }
        }
    }

    impl Match for FunctionToolResultMatcher {
        fn matches(&self, request: &WiremockRequest) -> bool {
            let Ok(body) = serde_json::from_slice::<serde_json::Value>(&request.body) else {
                return false;
            };
            body.get("input")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|items| {
                    items.iter().any(|item| {
                        item.get("type").and_then(serde_json::Value::as_str)
                            == Some("function_call_output")
                            && item.get("call_id").and_then(serde_json::Value::as_str)
                                == Some(self.call_id.as_str())
                            && item.get("output").and_then(serde_json::Value::as_str)
                                == Some(self.output.as_str())
                    })
                })
        }
    }

    #[derive(Debug)]
    struct InputTextMatcher(String);

    impl InputTextMatcher {
        fn new(text: &str) -> Self {
            Self(text.to_string())
        }
    }

    impl Match for InputTextMatcher {
        fn matches(&self, request: &WiremockRequest) -> bool {
            let Ok(body) = serde_json::from_slice::<serde_json::Value>(&request.body) else {
                return false;
            };
            body.get("input")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|items| {
                    items.iter().any(|item| {
                        item.get("type").and_then(serde_json::Value::as_str) == Some("message")
                            && item
                                .get("content")
                                .and_then(serde_json::Value::as_array)
                                .is_some_and(|content| {
                                    content.iter().any(|part| {
                                        part.get("text").and_then(serde_json::Value::as_str)
                                            == Some(self.0.as_str())
                                    })
                                })
                    })
                })
        }
    }

    async fn mount_delta_retry_backend_mocks(mock: &MockServer) {
        Mock::given(method("POST"))
            .and(path("/backend-api/codex/responses"))
            .and(PreviousResponseIdMatcher::some("resp_prev"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": { "message": "previous_response_id resp_prev not found" }
            })))
            .expect(1)
            .mount(mock)
            .await;

        Mock::given(method("POST"))
            .and(path("/backend-api/codex/responses"))
            .and(PreviousResponseIdMatcher::none())
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                success_sse("hello from fallback", "resp_new"),
                "text/event-stream",
            ))
            .expect(1)
            .mount(mock)
            .await;
    }

    fn success_sse(delta_text: &str, response_id: &str) -> String {
        format!(
            concat!(
                "event: response.output_text.delta\n",
                "data: {{\"type\":\"response.output_text.delta\",\"delta\":{delta:?}}}\n\n",
                "event: response.completed\n",
                "data: {{\"type\":\"response.completed\",\"response\":{{\"id\":{response_id:?},\"usage\":{{\"input_tokens\":1,\"output_tokens\":2,\"total_tokens\":3}}}}}}\n\n",
            ),
            delta = delta_text,
            response_id = response_id,
        )
    }

    fn function_tool_call_sse(
        call_id: &str,
        name: &str,
        arguments: &serde_json::Value,
        response_id: &str,
    ) -> String {
        format!(
            concat!(
                "event: response.output_item.done\n",
                "data: {{\"type\":\"response.output_item.done\",\"item\":{{\"type\":\"function_call\",\"call_id\":{call_id:?},\"name\":{name:?},\"arguments\":{arguments}}}}}\n\n",
                "event: response.completed\n",
                "data: {{\"type\":\"response.completed\",\"response\":{{\"id\":{response_id:?},\"usage\":{{\"input_tokens\":1,\"output_tokens\":2,\"total_tokens\":3}}}}}}\n\n",
            ),
            call_id = call_id,
            name = name,
            arguments = arguments,
            response_id = response_id,
        )
    }

    fn seed_incremental_branch(
        conversation_root: &std::path::Path,
        claude_session_id: &str,
    ) -> (ConversationStateStore, String) {
        let conversation_state = ConversationStateStore::new(conversation_root);
        let gateway_config = GatewayConfig::default();
        let request_messages = vec![AnthropicMessage {
            role: "user".to_string(),
            content: AnthropicContent::Text("hello".to_string()),
        }];
        let fingerprints = branch_fingerprints_from_messages(&request_messages, false);
        let request_compatibility_fingerprint =
            incremental_seed_request_compatibility_fingerprint(&gateway_config, &request_messages);
        let branch = conversation_state
            .create_branch(
                claude_session_id,
                &BranchCreateParams {
                    parent_branch_id: None,
                    fork_ancestor_checkpoint: None,
                    active_canonical_messages: Some(
                        serde_json::to_value(&request_messages)
                            .expect("serialize request messages"),
                    ),
                    fingerprints: fingerprints.clone(),
                },
            )
            .expect("create checkpointed branch");
        conversation_state
            .commit_turn(
                claude_session_id,
                &branch.branch_id,
                &CommitTurnParams {
                    turn_scope: ConversationTurnScope::Main,
                    turn_id: "seed-turn".to_string(),
                    fingerprints,
                    provider_response_id: Some("resp_prev".to_string()),
                    previous_response_id: Some("resp_seed".to_string()),
                    provider_model_fingerprint: Some(DEFAULT_BACKEND_MODEL.to_string()),
                    request_compatibility_fingerprint: Some(request_compatibility_fingerprint),
                    provider_output_items: Vec::new(),
                },
            )
            .expect("seed prior branch checkpoint");
        (conversation_state, branch.branch_id)
    }

    fn incremental_seed_request_compatibility_fingerprint(
        gateway_config: &GatewayConfig,
        request_messages: &[AnthropicMessage],
    ) -> String {
        let request = AnthropicMessagesRequest {
            model: DEFAULT_BACKEND_MODEL.to_string(),
            messages: request_messages.to_vec(),
            system: Vec::new(),
            stream: false,
            stop_sequences: Vec::new(),
            max_tokens: None,
            temperature: None,
            top_p: None,
            top_k: None,
            metadata: None,
            tools: Vec::new(),
            tool_choice: None,
            thinking: None,
            context_management: None,
            output_config: None,
        };
        let tool_context = ToolTranslationContext::default()
            .with_claude_code_config(gateway_config.workflow.claude_code.clone());
        let translated = translate_request_with_context(&request, &tool_context)
            .expect("translate seed request");
        let resolution = resolve_model(gateway_config, &request.model);
        request_compatibility_fingerprint(gateway_config, &resolution, &translated)
    }

    fn build_state_with_mode(
        base_url: &url::Url,
        auth_path: &std::path::Path,
        conversation_state: ConversationStateStore,
        mode: OpenAiIncrementalTransportMode,
    ) -> super::AppState {
        let mut gateway_config = GatewayConfig::default();
        gateway_config.workflow.conversation_state.enabled = true;
        gateway_config.providers.openai.incremental_transport.mode = mode;
        super::AppState {
            backend: gateway_backend_codex::client::CodexBackendClient::default()
                .with_base_url(base_url),
            auth_json_path: Some(auth_path.to_path_buf()),
            conversation_state,
            gateway_config,
            ..super::AppState::default()
        }
    }

    struct BranchRouteTestHarness {
        auth_path: std::path::PathBuf,
        conversation_root: std::path::PathBuf,
        claude_session_id: &'static str,
        branch_id: String,
        conversation_state: ConversationStateStore,
        base_url: url::Url,
        incremental_mode: OpenAiIncrementalTransportMode,
        state: super::AppState,
        mock: MockServer,
    }

    impl BranchRouteTestHarness {
        async fn new(conversation_prefix: &str) -> Self {
            Self::new_with_mode(conversation_prefix, OpenAiIncrementalTransportMode::Auto).await
        }

        async fn new_with_mode(
            conversation_prefix: &str,
            incremental_mode: OpenAiIncrementalTransportMode,
        ) -> Self {
            let auth_path = write_temp_auth_json();
            let conversation_root = std::env::temp_dir()
                .join(format!("{conversation_prefix}_{}", uuid::Uuid::new_v4()));
            let mock = MockServer::start().await;
            let claude_session_id = "session-1";
            let (conversation_state, branch_id) =
                seed_incremental_branch(&conversation_root, claude_session_id);
            let base_url = url::Url::parse(&mock.uri()).expect("mock url");
            let state = build_state_with_mode(
                &base_url,
                &auth_path,
                conversation_state.clone(),
                incremental_mode,
            );
            Self {
                auth_path,
                conversation_root,
                claude_session_id,
                branch_id,
                conversation_state,
                base_url,
                incremental_mode,
                state,
                mock,
            }
        }

        fn state(&self) -> &super::AppState {
            &self.state
        }

        fn claude_session_id(&self) -> &str {
            self.claude_session_id
        }

        fn branch(&self) -> BranchMetadata {
            self.conversation_state
                .load_branch(self.claude_session_id, &self.branch_id)
                .expect("reload branch metadata")
        }

        fn restarted_state(&self) -> super::AppState {
            let restarted_store = ConversationStateStore::new(&self.conversation_root);
            build_state_with_mode(
                &self.base_url,
                &self.auth_path,
                restarted_store,
                self.incremental_mode,
            )
        }

        fn branch_count(&self) -> usize {
            self.conversation_state
                .load_session(self.claude_session_id)
                .expect("reload session metadata")
                .branch_ids
                .len()
        }

        fn ledger(&self) -> String {
            std::fs::read_to_string(
                self.conversation_root
                    .join(format!("session-id-{}", self.claude_session_id))
                    .join(format!("tab-{}", self.branch_id))
                    .join("ledger.jsonl"),
            )
            .expect("read branch ledger")
        }
    }

    impl Drop for BranchRouteTestHarness {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.auth_path);
            let _ = std::fs::remove_dir_all(&self.conversation_root);
        }
    }

    fn assert_unary_text(json: &serde_json::Value, expected: &str) {
        assert_eq!(
            json.get("content")
                .and_then(serde_json::Value::as_array)
                .and_then(|items| items.first())
                .and_then(|item| item.get("text"))
                .and_then(serde_json::Value::as_str),
            Some(expected)
        );
    }

    fn assert_branch_checkpoint(
        branch: &BranchMetadata,
        expected_response_id: &str,
        expected_previous_response_id: Option<&str>,
    ) {
        assert_eq!(
            branch
                .openai_checkpoint
                .as_ref()
                .map(|checkpoint| checkpoint.response_id.as_str()),
            Some(expected_response_id)
        );
        assert_eq!(
            branch
                .openai_checkpoint
                .as_ref()
                .and_then(|checkpoint| checkpoint.previous_response_id.as_deref()),
            expected_previous_response_id
        );
    }

    async fn send_unary_message(
        state: &super::AppState,
        claude_session_id: Option<&str>,
        messages: serde_json::Value,
    ) -> serde_json::Value {
        let res = send_unary_message_response(state, claude_session_id, messages).await;
        assert!(
            res.status().is_success(),
            "expected success after full retry"
        );
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    async fn send_unary_message_response(
        state: &super::AppState,
        claude_session_id: Option<&str>,
        messages: serde_json::Value,
    ) -> axum::response::Response {
        let app = super::router(state.clone());
        let req_body = serde_json::json!({
            "model": DEFAULT_BACKEND_MODEL,
            "stream": false,
            "messages": messages
        });

        let mut builder = Request::builder()
            .method("POST")
            .uri("/v1/messages")
            .header("content-type", "application/json");
        if let Some(claude_session_id) = claude_session_id {
            builder = builder.header("x-claude-code-session-id", claude_session_id);
        }
        app.oneshot(builder.body(Body::from(req_body.to_string())).unwrap())
            .await
            .unwrap()
    }

    async fn send_streaming_message(
        state: &super::AppState,
        claude_session_id: Option<&str>,
        messages: serde_json::Value,
    ) -> Vec<(String, serde_json::Value)> {
        let app = super::router(state.clone());
        let req_body = serde_json::json!({
            "model": DEFAULT_BACKEND_MODEL,
            "stream": true,
            "messages": messages
        });

        let mut builder = Request::builder()
            .method("POST")
            .uri("/v1/messages")
            .header("content-type", "application/json");
        if let Some(claude_session_id) = claude_session_id {
            builder = builder.header("x-claude-code-session-id", claude_session_id);
        }
        let res = app
            .oneshot(builder.body(Body::from(req_body.to_string())).unwrap())
            .await
            .unwrap();

        assert!(
            res.status().is_success(),
            "expected successful SSE response"
        );
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let text = std::str::from_utf8(&body).expect("utf8 SSE body");
        parse_sse_frames(text)
    }

    fn mount_full_mode_unary_mock(text: &str, response_id: &str) -> Mock {
        Mock::given(method("POST"))
            .and(path("/backend-api/codex/responses"))
            .and(PreviousResponseIdMatcher::none())
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(success_sse(text, response_id), "text/event-stream"),
            )
    }

    fn inbound_snapshot_reconcile_count(ledger: &str) -> usize {
        ledger
            .matches("\"event_type\":\"inbound_canonical_snapshot_reconciled\"")
            .count()
    }

    fn write_session_updated_at(
        conversation_root: &std::path::Path,
        claude_session_id: &str,
        updated_at_unix_seconds: i64,
    ) {
        let session_path = conversation_root
            .join(format!("session-id-{claude_session_id}"))
            .join("session.json");
        let mut session: gateway_state::ClaudeSessionMetadata =
            serde_json::from_slice(&std::fs::read(&session_path).expect("read session.json"))
                .expect("decode session");
        session.updated_at_unix_seconds = updated_at_unix_seconds;
        std::fs::write(
            &session_path,
            serde_json::to_vec_pretty(&session).expect("encode session"),
        )
        .expect("write session");
    }

    #[tokio::test]
    async fn v1_messages_unary_emits_tool_use_block_from_backend() {
        if !wiremock_enabled() {
            return;
        }

        let auth_path = write_temp_auth_json();

        let mock = MockServer::start().await;
        let sse = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/streaming/backend_stream_tool_call.sse"
        ));
        Mock::given(method("POST"))
            .and(path("/backend-api/codex/responses"))
            .and(header("authorization", "Bearer tok_test"))
            .and(header("chatgpt-account-id", "acct_test"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(sse, "text/event-stream"))
            .mount(&mock)
            .await;

        let base_url = url::Url::parse(&mock.uri()).expect("mock url");
        let state = super::AppState {
            backend: gateway_backend_codex::client::CodexBackendClient::default()
                .with_base_url(&base_url),
            auth_json_path: Some(auth_path),
            ..super::AppState::default()
        };

        let app = super::router(state);
        let req_body = serde_json::json!({
            "model": DEFAULT_BACKEND_MODEL,
            "stream": false,
            "messages": [{ "role": "user", "content": "call a tool" }]
        });

        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert!(res.status().is_success());
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            json.get("stop_reason").and_then(|v| v.as_str()),
            Some("tool_use")
        );
        let block = json
            .get("content")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .expect("content[0]");
        assert_eq!(block.get("type").and_then(|v| v.as_str()), Some("tool_use"));
        assert_eq!(block.get("id").and_then(|v| v.as_str()), Some("call_1"));
        assert_eq!(block.get("name").and_then(|v| v.as_str()), Some("Read"));
    }

    #[tokio::test]
    async fn test_unary_message_missing_auth_returns_remediation() {
        let state = super::AppState {
            auth_json_path: Some(std::path::PathBuf::from("/nonexistent/auth.json")),
            ..super::AppState::default()
        };

        let app = super::router(state);
        let req_body = serde_json::json!({
            "model": DEFAULT_BACKEND_MODEL,
            "stream": false,
            "messages": [{ "role": "user", "content": "hello" }]
        });

        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(
            json.get("error")
                .and_then(|e| e.get("type"))
                .and_then(|v| v.as_str()),
            Some("auth_error")
        );
        assert_eq!(
            json.get("error")
                .and_then(|e| e.get("auth_remediation"))
                .and_then(|v| v.as_str()),
            Some("Please run: cld-gateway login claude")
        );
    }

    #[tokio::test]
    async fn test_streaming_message_missing_auth_returns_remediation() {
        let state = super::AppState {
            auth_json_path: Some(std::path::PathBuf::from("/nonexistent/auth.json")),
            ..super::AppState::default()
        };

        let app = super::router(state);
        let req_body = serde_json::json!({
            "model": DEFAULT_BACKEND_MODEL,
            "stream": true,
            "messages": [{ "role": "user", "content": "hello" }]
        });

        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(res.status(), StatusCode::OK);
        let content_type = res
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert!(content_type.contains("text/event-stream"));

        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let text = std::str::from_utf8(&body).unwrap();
        let events = parse_sse_frames(text);

        assert!(!events.is_empty());
        let (first_event, first_data) = &events[0];
        assert_eq!(first_event, "error");
        assert_eq!(
            first_data
                .get("error")
                .and_then(|e| e.get("type"))
                .and_then(|v| v.as_str()),
            Some("auth_error")
        );
        assert_eq!(
            first_data
                .get("error")
                .and_then(|e| e.get("auth_remediation"))
                .and_then(|v| v.as_str()),
            Some("Please run: cld-gateway login claude")
        );
    }

    #[tokio::test]
    async fn test_auth_status_missing_auth_returns_remediation() {
        let auth_path = std::env::temp_dir().join(format!(
            "gateway_missing_auth_{}.json",
            uuid::Uuid::new_v4()
        ));
        let state = super::AppState {
            auth_json_path: Some(auth_path),
            ..super::AppState::default()
        };
        let app = super::router(state);
        let res = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/auth/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(res.status(), StatusCode::OK);
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(
            json.get("logged_in").and_then(serde_json::Value::as_bool),
            Some(false)
        );
        assert_eq!(
            json.get("auth_remediation").and_then(|v| v.as_str()),
            Some("Please run: cld-gateway login claude")
        );
    }

    #[tokio::test]
    async fn test_refresh_failure_returns_remediation() {
        if !wiremock_enabled() {
            return;
        }

        let auth_path = write_temp_auth_json();
        let mock = MockServer::start().await;

        // Mock any POST to codex/responses to return 401
        Mock::given(method("POST"))
            .and(path("/backend-api/codex/responses"))
            .respond_with(
                ResponseTemplate::new(401)
                    .set_body_json(serde_json::json!({"error": "refresh_required"})),
            )
            .mount(&mock)
            .await;

        let base_url = url::Url::parse(&mock.uri()).expect("mock url");
        let state = super::AppState {
            backend: gateway_backend_codex::client::CodexBackendClient::default()
                .with_base_url(&base_url),
            auth_json_path: Some(auth_path.clone()),
            ..super::AppState::default()
        };

        let app = super::router(state);
        let req_body = serde_json::json!({
            "model": DEFAULT_BACKEND_MODEL,
            "stream": false,
            "messages": [{ "role": "user", "content": "hello" }]
        });

        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        // When backend returns 401, gateway should return 401 with remediation
        assert_eq!(
            res.status(),
            StatusCode::UNAUTHORIZED,
            "expected 401 when backend returns 401, got {}",
            res.status()
        );
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(
            json.get("error")
                .and_then(|e| e.get("type"))
                .and_then(|v| v.as_str()),
            Some("auth_error"),
            "expected auth_error type in response, got: {json:?}"
        );
        assert_eq!(
            json.get("error")
                .and_then(|e| e.get("auth_remediation"))
                .and_then(|v| v.as_str()),
            Some("Please run: cld-gateway login claude"),
            "expected remediation message in error response, got: {json:?}"
        );

        let _ = std::fs::remove_file(&auth_path);
    }

    #[tokio::test]
    async fn test_refresh_failure_returns_remediation_streaming() {
        if !wiremock_enabled() {
            return;
        }

        let auth_path = write_temp_auth_json();
        let mock = MockServer::start().await;

        // Mock any POST to codex/responses to return 401
        Mock::given(method("POST"))
            .and(path("/backend-api/codex/responses"))
            .respond_with(
                ResponseTemplate::new(401)
                    .set_body_json(serde_json::json!({"error": "refresh_required"})),
            )
            .mount(&mock)
            .await;

        let base_url = url::Url::parse(&mock.uri()).expect("mock url");
        let state = super::AppState {
            backend: gateway_backend_codex::client::CodexBackendClient::default()
                .with_base_url(&base_url),
            auth_json_path: Some(auth_path.clone()),
            ..super::AppState::default()
        };

        let app = super::router(state);
        let req_body = serde_json::json!({
            "model": DEFAULT_BACKEND_MODEL,
            "stream": true,
            "messages": [{ "role": "user", "content": "hello" }]
        });

        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        // When backend returns 401, gateway should return 200 (SSE stream starts)
        // but the first event should be an error with structured auth remediation.
        assert_eq!(
            res.status(),
            StatusCode::OK,
            "expected 200 for SSE stream, got {}",
            res.status()
        );

        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let body_str = std::str::from_utf8(&body).unwrap();
        let events = parse_sse_frames(body_str);

        assert!(
            !events.is_empty(),
            "expected at least one SSE event, got empty stream"
        );
        let (first_event, first_payload) = &events[0];
        assert_eq!(first_event, "error");
        assert_eq!(
            first_payload
                .get("error")
                .and_then(|e| e.get("type"))
                .and_then(|v| v.as_str()),
            Some("auth_error"),
            "expected structured auth_error payload, got: {first_payload:?}"
        );
        assert_eq!(
            first_payload
                .get("error")
                .and_then(|e| e.get("auth_remediation"))
                .and_then(|v| v.as_str()),
            Some("Please run: cld-gateway login claude"),
            "expected structured remediation in SSE payload, got: {first_payload:?}"
        );

        let _ = std::fs::remove_file(&auth_path);
    }

    #[tokio::test]
    async fn unary_delta_rejection_invalidates_checkpoint_and_retries_full_once() {
        if !wiremock_enabled() {
            return;
        }
        let harness = BranchRouteTestHarness::new("gateway_conversation_state").await;
        mount_delta_retry_backend_mocks(&harness.mock).await;
        let json = send_unary_message(
            harness.state(),
            Some(harness.claude_session_id()),
            serde_json::json!([{ "role": "user", "content": "hello" }]),
        )
        .await;
        assert_unary_text(&json, "hello from fallback");
        assert_branch_checkpoint(&harness.branch(), "resp_new", None);
    }

    #[tokio::test]
    async fn compact_history_forces_one_full_request_and_clears_reset_after_success() {
        if !wiremock_enabled() {
            return;
        }
        let harness = BranchRouteTestHarness::new("gateway_compaction_state").await;
        Mock::given(method("POST"))
            .and(path("/backend-api/codex/responses"))
            .and(PreviousResponseIdMatcher::none())
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                success_sse("hello after compaction", "resp_after_compaction"),
                "text/event-stream",
            ))
            .expect(1)
            .mount(&harness.mock)
            .await;

        let json = send_unary_message(
            harness.state(),
            Some(harness.claude_session_id()),
            serde_json::json!([
                {
                    "role": "user",
                    "content": "<command-message>compact</command-message>\n<command-name>/compact</command-name>\n<command-args></command-args>\n<local-command-stdout>Compacted </local-command-stdout>"
                },
                { "role": "user", "content": "hello" }
            ]),
        )
        .await;

        assert_unary_text(&json, "hello after compaction");
        let branch = harness.branch();
        assert!(!branch.compaction_reset_pending);
        assert_branch_checkpoint(&branch, "resp_after_compaction", None);
        let ledger = harness.ledger();
        assert!(ledger.contains("\"event_type\":\"compaction_applied\""));
    }

    #[tokio::test]
    async fn side_turn_does_not_advance_main_branch_checkpoint() {
        if !wiremock_enabled() {
            return;
        }
        let harness = BranchRouteTestHarness::new("gateway_side_turn_state").await;
        Mock::given(method("POST"))
            .and(path("/backend-api/codex/responses"))
            .and(PreviousResponseIdMatcher::none())
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                success_sse("side answer", "resp_side_turn"),
                "text/event-stream",
            ))
            .expect(1)
            .mount(&harness.mock)
            .await;

        let side_turn = concat!(
            "This is a side question from the user.\n",
            "Please use a separate, lightweight agent to answer.\n",
            "The main agent is NOT interrupted.\n",
            "What does this code path do?"
        );

        let json = send_unary_message(
            harness.state(),
            Some(harness.claude_session_id()),
            serde_json::json!([
                { "role": "user", "content": "hello" },
                { "role": "user", "content": side_turn }
            ]),
        )
        .await;

        assert_unary_text(&json, "side answer");

        let branch = harness.branch();
        assert_eq!(branch.current_checkpoint_id.as_deref(), Some("resp_prev"));
        assert_eq!(branch.last_main_turn_id.as_deref(), Some("seed-turn"));
        assert_eq!(
            branch.active_canonical_messages,
            Some(serde_json::json!([{ "role": "user", "content": "hello" }]))
        );
    }

    #[tokio::test]
    async fn full_mode_restart_replay_reuses_same_branch_without_duplicate_snapshot_events() {
        if !wiremock_enabled() {
            return;
        }
        let harness = BranchRouteTestHarness::new_with_mode(
            "gateway_full_mode_restart_replay",
            OpenAiIncrementalTransportMode::AlwaysFull,
        )
        .await;
        mount_full_mode_unary_mock("full mode answer", "resp_full_mode")
            .expect(2)
            .mount(&harness.mock)
            .await;

        let messages = serde_json::json!([{ "role": "user", "content": "hello" }]);
        let first = send_unary_message(
            harness.state(),
            Some(harness.claude_session_id()),
            messages.clone(),
        )
        .await;
        assert_unary_text(&first, "full mode answer");

        let restarted_state = harness.restarted_state();
        let second = send_unary_message(
            &restarted_state,
            Some(harness.claude_session_id()),
            messages,
        )
        .await;
        assert_unary_text(&second, "full mode answer");

        assert_eq!(harness.branch_count(), 1);
        assert_eq!(inbound_snapshot_reconcile_count(&harness.ledger()), 0);
    }

    #[tokio::test]
    async fn full_mode_side_turn_restart_keeps_main_checkpoint_and_snapshot() {
        if !wiremock_enabled() {
            return;
        }
        let harness = BranchRouteTestHarness::new_with_mode(
            "gateway_full_mode_side_restart",
            OpenAiIncrementalTransportMode::AlwaysFull,
        )
        .await;
        mount_full_mode_unary_mock("side answer", "resp_side_restart")
            .expect(1)
            .mount(&harness.mock)
            .await;

        let restarted_state = harness.restarted_state();
        let side_turn = concat!(
            "This is a side question from the user.\n",
            "Please use a separate, lightweight agent to answer.\n",
            "The main agent is NOT interrupted.\n",
            "What does this code path do?"
        );

        let json = send_unary_message(
            &restarted_state,
            Some(harness.claude_session_id()),
            serde_json::json!([
                { "role": "user", "content": "hello" },
                { "role": "user", "content": side_turn }
            ]),
        )
        .await;

        assert_unary_text(&json, "side answer");
        let branch = harness.branch();
        assert_eq!(branch.current_checkpoint_id.as_deref(), Some("resp_prev"));
        assert_eq!(branch.last_main_turn_id.as_deref(), Some("seed-turn"));
        assert_eq!(
            branch.active_canonical_messages,
            Some(serde_json::json!([{ "role": "user", "content": "hello" }]))
        );
    }

    #[tokio::test]
    async fn full_mode_compaction_restart_replaces_active_state_safely() {
        if !wiremock_enabled() {
            return;
        }
        let harness = BranchRouteTestHarness::new_with_mode(
            "gateway_full_mode_compaction_restart",
            OpenAiIncrementalTransportMode::AlwaysFull,
        )
        .await;
        mount_full_mode_unary_mock("hello after full compaction", "resp_full_compaction")
            .expect(1)
            .mount(&harness.mock)
            .await;

        let restarted_state = harness.restarted_state();
        let messages = serde_json::json!([
            {
                "role": "user",
                "content": "<command-message>compact</command-message>\n<command-name>/compact</command-name>\n<command-args></command-args>\n<local-command-stdout>Compacted </local-command-stdout>"
            },
            { "role": "user", "content": "hello" }
        ]);
        let json = send_unary_message(
            &restarted_state,
            Some(harness.claude_session_id()),
            messages.clone(),
        )
        .await;

        assert_unary_text(&json, "hello after full compaction");
        let branch = harness.branch();
        assert!(!branch.compaction_reset_pending);
        assert_branch_checkpoint(&branch, "resp_full_compaction", None);
        assert_eq!(
            branch.active_canonical_messages,
            Some(serde_json::json!([
                { "role": "user", "content": "" },
                { "role": "user", "content": "hello" }
            ]))
        );
        assert!(
            harness
                .ledger()
                .contains("\"event_type\":\"compaction_applied\"")
        );
    }

    #[tokio::test]
    async fn generic_backend_error_does_not_retry_full_or_advance_branch_state() {
        if !wiremock_enabled() {
            return;
        }
        let harness = BranchRouteTestHarness::new("gateway_generic_backend_error").await;
        Mock::given(method("POST"))
            .and(path("/backend-api/codex/responses"))
            .and(PreviousResponseIdMatcher::some("resp_prev"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": { "message": "tool schema validation failed" }
            })))
            .expect(1)
            .mount(&harness.mock)
            .await;

        let response = send_unary_message_response(
            harness.state(),
            Some(harness.claude_session_id()),
            serde_json::json!([{ "role": "user", "content": "hello" }]),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let branch = harness.branch();
        assert_branch_checkpoint(&branch, "resp_prev", Some("resp_seed"));
        assert_eq!(branch.last_main_turn_id.as_deref(), Some("seed-turn"));
    }

    #[tokio::test]
    async fn interrupted_stream_does_not_advance_branch_state() {
        if !wiremock_enabled() {
            return;
        }
        let harness = BranchRouteTestHarness::new("gateway_interrupted_stream").await;
        Mock::given(method("POST"))
            .and(path("/backend-api/codex/responses"))
            .and(PreviousResponseIdMatcher::some("resp_prev"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                concat!(
                    "event: response.output_text.delta\n",
                    "data: {\"type\":\"response.output_text.delta\",\"delta\":\"partial\"}\n\n"
                ),
                "text/event-stream",
            ))
            .expect(1)
            .mount(&harness.mock)
            .await;

        let events = send_streaming_message(
            harness.state(),
            Some(harness.claude_session_id()),
            serde_json::json!([{ "role": "user", "content": "hello" }]),
        )
        .await;

        assert!(!events.is_empty(), "expected partial SSE events");
        let branch = harness.branch();
        assert_branch_checkpoint(&branch, "resp_prev", Some("resp_seed"));
        assert_eq!(branch.last_main_turn_id.as_deref(), Some("seed-turn"));
    }

    #[test]
    fn conversation_state_store_startup_cleanup_applies_retention_config() {
        let conversation_root =
            std::env::temp_dir().join(format!("gateway_startup_cleanup_{}", uuid::Uuid::new_v4()));
        let store = ConversationStateStore::new(&conversation_root);
        let session = store.ensure_session("session-1").expect("create session");
        write_session_updated_at(
            &conversation_root,
            "session-1",
            session.created_at_unix_seconds - (10 * 24 * 60 * 60),
        );

        let mut config = GatewayConfig::default();
        config.workflow.conversation_state.enabled = true;
        config.workflow.conversation_state.persistence_root = Some(conversation_root.clone());
        config
            .workflow
            .conversation_state
            .retention
            .max_session_age_days = Some(7);

        let cleaned_store = super::conversation_state_store_for_config(&config);
        assert_eq!(cleaned_store.root(), conversation_root.as_path());
        assert!(!conversation_root.join("session-id-session-1").exists());

        let _ = std::fs::remove_dir_all(&conversation_root);
    }

    #[test]
    fn branch_fingerprints_are_deterministic_for_identical_messages() {
        let messages = vec![
            AnthropicMessage {
                role: "user".to_string(),
                content: AnthropicContent::Text("hello".to_string()),
            },
            AnthropicMessage {
                role: "assistant".to_string(),
                content: AnthropicContent::Text("world".to_string()),
            },
        ];

        let first = branch_fingerprints_from_messages(&messages, false);
        let second = branch_fingerprints_from_messages(&messages, false);
        assert_eq!(first, second);
    }

    #[test]
    fn branch_fingerprints_change_when_recent_tail_changes() {
        let base = vec![AnthropicMessage {
            role: "user".to_string(),
            content: AnthropicContent::Text("hello".to_string()),
        }];
        let mut changed = base.clone();
        changed.push(AnthropicMessage {
            role: "assistant".to_string(),
            content: AnthropicContent::Text("different".to_string()),
        });

        let first = branch_fingerprints_from_messages(&base, false);
        let second = branch_fingerprints_from_messages(&changed, false);
        assert_ne!(
            first.recent_message_tail_hash,
            second.recent_message_tail_hash
        );
        assert_ne!(first.branch_state_hash, second.branch_state_hash);
    }

    #[test]
    fn persisted_canonical_messages_render_same_backend_request_as_direct_request() {
        let req = AnthropicMessagesRequest {
            model: DEFAULT_BACKEND_MODEL.to_string(),
            messages: vec![
                AnthropicMessage {
                    role: "user".to_string(),
                    content: AnthropicContent::Text("hello".to_string()),
                },
                AnthropicMessage {
                    role: "assistant".to_string(),
                    content: AnthropicContent::Blocks(vec![AnthropicContentBlock {
                        block_type: "tool_use".to_string(),
                        text: None,
                        id: Some("call_1".to_string()),
                        name: Some("Read".to_string()),
                        input: Some(serde_json::json!({"file_path":"/tmp/a.txt"})),
                        tool_use_id: None,
                        content: None,
                        is_error: None,
                        source: None,
                        extra: std::collections::BTreeMap::new(),
                    }]),
                },
            ],
            system: vec![crate::types::AnthropicSystemBlock {
                block_type: "text".to_string(),
                text: Some("system".to_string()),
            }],
            stream: false,
            stop_sequences: Vec::new(),
            max_tokens: None,
            temperature: None,
            top_p: None,
            top_k: None,
            metadata: None,
            tools: Vec::new(),
            tool_choice: None,
            thinking: None,
            context_management: None,
            output_config: None,
        };

        let direct_context = build_tool_translation_context(&super::AppState::default(), &req);
        let direct = translate_request_with_context(&req, &direct_context).expect("direct render");

        let persisted_req = super::request_with_messages(&req, req.messages.clone());
        let persisted_context =
            build_tool_translation_context(&super::AppState::default(), &persisted_req);
        let persisted = translate_request_with_context(&persisted_req, &persisted_context)
            .expect("persisted render");

        assert_eq!(direct.instructions, persisted.instructions);
        assert_eq!(direct.input, persisted.input);
        assert_eq!(direct.tools, persisted.tools);
        assert_eq!(direct.tool_choice, persisted.tool_choice);
        assert_eq!(direct.parallel_tool_calls, persisted.parallel_tool_calls);
        assert_eq!(direct.text, persisted.text);
        assert_eq!(direct.reasoning, persisted.reasoning);
        assert_eq!(direct.include, persisted.include);
        assert_eq!(direct.client_metadata, persisted.client_metadata);
    }

    #[tokio::test]
    async fn streaming_main_turn_commits_branch_checkpoint_on_completed_event() {
        if !wiremock_enabled() {
            return;
        }
        let harness = BranchRouteTestHarness::new("gateway_stream_commit_state").await;
        Mock::given(method("POST"))
            .and(path("/backend-api/codex/responses"))
            .and(PreviousResponseIdMatcher::some("resp_prev"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                success_sse("hello from stream", "resp_stream_next"),
                "text/event-stream",
            ))
            .expect(1)
            .mount(&harness.mock)
            .await;

        let events = send_streaming_message(
            harness.state(),
            Some(harness.claude_session_id()),
            serde_json::json!([{ "role": "user", "content": "hello" }]),
        )
        .await;

        assert!(
            events.iter().any(|(event, _)| event == "message_stop"),
            "expected completed SSE message_stop event"
        );

        let branch = harness.branch();
        assert_eq!(
            branch.current_checkpoint_id.as_deref(),
            Some("resp_stream_next")
        );
        assert_branch_checkpoint(&branch, "resp_stream_next", Some("resp_prev"));
    }

    #[tokio::test]
    async fn stored_tool_call_kind_is_used_for_followup_tool_result_requests() {
        if !wiremock_enabled() {
            return;
        }
        let auth_path = write_temp_auth_json();
        let tool_calls_path = std::env::temp_dir().join(format!(
            "gateway_tool_continuity_{}.sqlite",
            uuid::Uuid::new_v4()
        ));
        let mock = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/backend-api/codex/responses"))
            .and(InputTextMatcher::new("hello"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                function_tool_call_sse(
                    "call_1",
                    "Read",
                    &serde_json::json!({"file_path": "/tmp/a.txt"}),
                    "resp_tool",
                ),
                "text/event-stream",
            ))
            .expect(1)
            .mount(&mock)
            .await;

        Mock::given(method("POST"))
            .and(path("/backend-api/codex/responses"))
            .and(FunctionToolResultMatcher::new("call_1", "done"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                success_sse("tool result processed", "resp_after_tool"),
                "text/event-stream",
            ))
            .expect(1)
            .mount(&mock)
            .await;

        let base_url = url::Url::parse(&mock.uri()).expect("mock url");
        let state = super::AppState {
            backend: gateway_backend_codex::client::CodexBackendClient::default()
                .with_base_url(&base_url),
            auth_json_path: Some(auth_path.clone()),
            tool_calls: ToolCallStore::new(&tool_calls_path),
            ..super::AppState::default()
        };

        let first = send_unary_message(
            &state,
            None,
            serde_json::json!([{ "role": "user", "content": "hello" }]),
        )
        .await;
        assert_eq!(
            first.get("stop_reason").and_then(serde_json::Value::as_str),
            Some("tool_use")
        );

        let second = send_unary_message(
            &state,
            None,
            serde_json::json!([
                {
                    "role": "assistant",
                    "content": [{
                        "type": "tool_use",
                        "id": "call_1",
                        "name": "Read",
                        "input": { "file_path": "/tmp/a.txt" }
                    }]
                },
                {
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": "call_1",
                        "content": "done"
                    }]
                }
            ]),
        )
        .await;

        assert_eq!(
            second
                .get("content")
                .and_then(serde_json::Value::as_array)
                .and_then(|items| items.first())
                .and_then(|item| item.get("text"))
                .and_then(serde_json::Value::as_str),
            Some("tool result processed")
        );

        let _ = std::fs::remove_file(&auth_path);
        let _ = std::fs::remove_file(&tool_calls_path);
    }

    #[tokio::test]
    async fn v1_messages_status_command_routes_through_executor() {
        if !wiremock_enabled() {
            return;
        }

        let auth_path = write_temp_auth_json();
        let mock = MockServer::start().await;

        // The backend should receive the executor-enriched request.
        // We verify the request body contains executor-produced JSON.
        let backend_response = format!("{}\n\n", fixture("streaming/backend_stream_text_only.sse"));
        Mock::given(method("POST"))
            .and(path("/backend-api/codex/responses"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(backend_response, "text/event-stream"),
            )
            .expect(1)
            .mount(&mock)
            .await;

        // Also mock the usage endpoint (executor will try to fetch it)
        Mock::given(method("GET"))
            .and(path("/api/codex/usage"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "plan_type": "pro",
                "rate_limit": { "allowed": true, "limit_reached": false }
            })))
            .mount(&mock)
            .await;

        let base_url = url::Url::parse(&mock.uri()).expect("mock url");
        let state = super::AppState {
            backend: gateway_backend_codex::client::CodexBackendClient::default()
                .with_base_url(&base_url),
            auth_json_path: Some(auth_path.clone()),
            ..super::AppState::default()
        };

        let app = super::router(state);
        let req_body = serde_json::json!({
            "model": DEFAULT_BACKEND_MODEL,
            "stream": true,
            "messages": [{
                "role": "user",
                "content": "<command-message>status</command-message>\n<command-name>/status</command-name>\n<command-args></command-args>"
            }]
        });

        let res = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/messages")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        // The handler should succeed (executor ran, then forwarded to backend)
        assert!(
            res.status().is_success(),
            "expected success status, got {}",
            res.status()
        );

        // Verify the backend received exactly one request (executor path completed
        // and forwarded the enriched request to the backend)
        // The Mock::expect(1) above ensures this.

        let _ = std::fs::remove_file(&auth_path);
    }
}

#[cfg(test)]
mod models_api_tests {
    use super::{AppState, router};
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use tower::ServiceExt as _;
    #[tokio::test]
    async fn v1_models_reads_local_settings_catalog() {
        let settings_path = std::env::temp_dir().join(format!(
            "claude_gateway_settings_{}.json",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(
            &settings_path,
            serde_json::to_string_pretty(&serde_json::json!({
                "includeCoAuthoredBy": false,
                "models": [
                    {
                        "id": "gpt-5.4-mini",
                        "name": "GPT-5.4 Mini",
                        "description": "OpenAI small/fallback model"
                    },
                    {
                        "id": "gpt-5.4",
                        "name": "GPT-5.4",
                        "description": "OpenAI general-purpose model"
                    }
                ]
            }))
            .expect("serialize temp settings"),
        )
        .expect("write temp settings");

        let state = AppState::default().with_claude_gateway_settings_path(settings_path.clone());

        let app = router(state);
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/v1/models")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert!(res.status().is_success());
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let ids: Vec<String> = json["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["id"].as_str().unwrap().to_string())
            .collect();

        assert_eq!(ids, vec!["gpt-5.4-mini".to_string(), "gpt-5.4".to_string()]);
        assert_eq!(json["data"][0]["name"].as_str(), Some("GPT-5.4 Mini"));
        assert_eq!(
            json["data"][1]["description"].as_str(),
            Some("OpenAI general-purpose model")
        );

        let _ = std::fs::remove_file(&settings_path);
    }
}

#[cfg(test)]
mod transport_selection_tests {
    use super::{
        BranchSelectionAction, PreparedConversationBranch, SelectedTransport, TransportMode,
        is_known_delta_rejection, select_transport,
    };
    use gateway_backend_codex::client::BackendError;
    use gateway_core::config::{
        GatewayConfig, OpenAiIncrementalTransportConfig, OpenAiIncrementalTransportMode,
        OpenAiProviderConfig, ProviderConfigs,
    };
    use gateway_state::{BranchMetadata, ConversationTurnScope, OpenAiCheckpoint};

    fn config_with_mode(mode: OpenAiIncrementalTransportMode) -> GatewayConfig {
        GatewayConfig {
            providers: ProviderConfigs {
                openai: OpenAiProviderConfig {
                    incremental_transport: OpenAiIncrementalTransportConfig { mode },
                    ..OpenAiProviderConfig::default()
                },
            },
            ..GatewayConfig::default()
        }
    }

    fn prepared_branch(
        turn_scope: ConversationTurnScope,
        openai_checkpoint: Option<OpenAiCheckpoint>,
        compaction_reset_pending: bool,
    ) -> PreparedConversationBranch {
        PreparedConversationBranch {
            claude_session_id: "session-1".to_string(),
            branch: BranchMetadata {
                schema_version: 1,
                branch_id: "branch-1".to_string(),
                parent_branch_id: None,
                fork_ancestor_checkpoint: None,
                current_checkpoint_id: Some("checkpoint-1".to_string()),
                active_canonical_messages: None,
                fingerprints: gateway_state::BranchFingerprintSet::default(),
                openai_checkpoint,
                compaction_reset_pending,
                last_main_turn_id: Some("turn-1".to_string()),
                created_at_unix_seconds: 0,
                updated_at_unix_seconds: 0,
            },
            selection_action: BranchSelectionAction::ContinuedExisting,
            turn_scope,
            fingerprints: gateway_state::BranchFingerprintSet::default(),
            active_messages: Vec::new(),
        }
    }

    #[test]
    fn auto_mode_without_branch_uses_full_transport() {
        let selected = select_transport(
            &config_with_mode(OpenAiIncrementalTransportMode::Auto),
            None,
            "gpt-5",
            "fp-1",
        )
        .expect("auto mode should fall back to full transport");

        assert_eq!(
            selected,
            SelectedTransport {
                mode: TransportMode::Full,
                previous_response_id: None,
                reason: "no_branch_available",
            }
        );
    }

    #[test]
    fn auto_mode_with_matching_checkpoint_uses_incremental_transport() {
        let selected = select_transport(
            &config_with_mode(OpenAiIncrementalTransportMode::Auto),
            Some(&prepared_branch(
                ConversationTurnScope::Main,
                Some(OpenAiCheckpoint {
                    response_id: "resp_123".to_string(),
                    previous_response_id: Some("resp_122".to_string()),
                    provider_model_fingerprint: "gpt-5".to_string(),
                    request_compatibility_fingerprint: Some("fp-1".to_string()),
                }),
                false,
            )),
            "gpt-5",
            "fp-1",
        )
        .expect("matching checkpoint should enable incremental transport");

        assert_eq!(
            selected,
            SelectedTransport {
                mode: TransportMode::Incremental,
                previous_response_id: Some("resp_123".to_string()),
                reason: "branch_checkpoint_reuse",
            }
        );
    }

    #[test]
    fn auto_mode_forces_full_transport_when_compaction_reset_is_pending() {
        let selected = select_transport(
            &config_with_mode(OpenAiIncrementalTransportMode::Auto),
            Some(&prepared_branch(
                ConversationTurnScope::Main,
                Some(OpenAiCheckpoint {
                    response_id: "resp_123".to_string(),
                    previous_response_id: Some("resp_122".to_string()),
                    provider_model_fingerprint: "gpt-5".to_string(),
                    request_compatibility_fingerprint: Some("fp-1".to_string()),
                }),
                true,
            )),
            "gpt-5",
            "fp-1",
        )
        .expect("auto mode should fall back to full transport after compaction");

        assert_eq!(
            selected,
            SelectedTransport {
                mode: TransportMode::Full,
                previous_response_id: None,
                reason: "compaction_reset_pending",
            }
        );
    }

    #[test]
    fn require_delta_without_checkpoint_returns_error() {
        let err = select_transport(
            &config_with_mode(OpenAiIncrementalTransportMode::RequireDelta),
            Some(&prepared_branch(ConversationTurnScope::Main, None, false)),
            "gpt-5",
            "fp-1",
        )
        .expect_err("require_delta must reject branches without checkpoints");

        assert_eq!(
            err,
            "incremental transport is required, but this branch has no stored OpenAI checkpoint"
        );
    }

    #[test]
    fn require_delta_rejects_side_turns() {
        let err = select_transport(
            &config_with_mode(OpenAiIncrementalTransportMode::RequireDelta),
            Some(&prepared_branch(
                ConversationTurnScope::Side,
                Some(OpenAiCheckpoint {
                    response_id: "resp_123".to_string(),
                    previous_response_id: Some("resp_122".to_string()),
                    provider_model_fingerprint: "gpt-5".to_string(),
                    request_compatibility_fingerprint: Some("fp-1".to_string()),
                }),
                false,
            )),
            "gpt-5",
            "fp-1",
        )
        .expect_err("require_delta must reject side turns");

        assert_eq!(
            err,
            "incremental transport is required, but side-turn requests are not eligible for previous_response_id reuse"
        );
    }

    #[test]
    fn always_full_ignores_available_checkpoint() {
        let selected = select_transport(
            &config_with_mode(OpenAiIncrementalTransportMode::AlwaysFull),
            Some(&prepared_branch(
                ConversationTurnScope::Main,
                Some(OpenAiCheckpoint {
                    response_id: "resp_123".to_string(),
                    previous_response_id: Some("resp_122".to_string()),
                    provider_model_fingerprint: "gpt-5".to_string(),
                    request_compatibility_fingerprint: Some("fp-1".to_string()),
                }),
                false,
            )),
            "gpt-5",
            "fp-1",
        )
        .expect("always_full should not error when checkpoint exists");

        assert_eq!(
            selected,
            SelectedTransport {
                mode: TransportMode::Full,
                previous_response_id: None,
                reason: "always_full_mode",
            }
        );
    }

    #[test]
    fn auto_mode_forces_full_transport_when_request_compatibility_changes() {
        let selected = select_transport(
            &config_with_mode(OpenAiIncrementalTransportMode::Auto),
            Some(&prepared_branch(
                ConversationTurnScope::Main,
                Some(OpenAiCheckpoint {
                    response_id: "resp_123".to_string(),
                    previous_response_id: Some("resp_122".to_string()),
                    provider_model_fingerprint: "gpt-5".to_string(),
                    request_compatibility_fingerprint: Some("fp-old".to_string()),
                }),
                false,
            )),
            "gpt-5",
            "fp-new",
        )
        .expect("auto mode should fall back to full transport on compatibility mismatch");

        assert_eq!(
            selected,
            SelectedTransport {
                mode: TransportMode::Full,
                previous_response_id: None,
                reason: "request_compatibility_mismatch",
            }
        );
    }

    #[test]
    fn delta_rejection_detection_accepts_known_previous_response_failure() {
        let err = BackendError::UnexpectedStatusWithBody {
            status: 400,
            body: "previous_response_id resp_prev not found".to_string(),
        };

        assert!(is_known_delta_rejection(&err, Some("resp_prev")));
    }

    #[test]
    fn delta_rejection_detection_rejects_auth_failures() {
        let err = BackendError::UnexpectedStatusWithBody {
            status: 401,
            body: "previous_response_id resp_prev not found".to_string(),
        };

        assert!(!is_known_delta_rejection(&err, Some("resp_prev")));
    }

    #[test]
    fn delta_rejection_detection_rejects_generic_backend_bad_requests() {
        let err = BackendError::UnexpectedStatusWithBody {
            status: 400,
            body: "tool schema validation failed".to_string(),
        };

        assert!(!is_known_delta_rejection(&err, Some("resp_prev")));
    }
}
