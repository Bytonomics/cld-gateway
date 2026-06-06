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
use gateway_core::Secret;
use gateway_core::config::{
    GatewayConfig, ModelResolution, load_gateway_config_default_path, resolve_model,
    service_tier_for_config,
};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::{info, warn};
use uuid::Uuid;

#[cfg(test)]
use std::path::PathBuf;

mod sse_bridge;
mod tool_arg_policy;
mod translate;
mod types;

use crate::translate::{ToolTranslationContext, translate_request_with_context};
use crate::types::AnthropicMessagesRequest;
use gateway_state::ToolCallStore;

#[derive(Clone, Default)]
pub struct AppState {
    auth: gateway_auth_codex::CodexAuthManager,
    backend: gateway_backend_codex::client::CodexBackendClient,
    openai_models_url: String,
    openai_api_key: Option<Secret<String>>,
    tool_calls: ToolCallStore,
    gateway_config: GatewayConfig,
    #[cfg(test)]
    auth_json_path: Option<PathBuf>,
}

impl AppState {
    /// # Errors
    ///
    /// Returns an error if the gateway config file exists but cannot be read or parsed.
    pub fn from_env() -> Result<Self, gateway_core::config::GatewayConfigError> {
        let gateway_config = load_gateway_config_default_path()?;
        let openai_api_key = std::env::var("OPENAI_API_KEY")
            .ok()
            .map(Secret::new)
            .or_else(|| {
                gateway_auth_codex::load_openai_api_key_default_path()
                    .ok()
                    .flatten()
            });
        let backend = backend_client_from_env();
        Ok(Self {
            backend,
            openai_models_url: "https://api.openai.com/v1/models".to_string(),
            openai_api_key,
            gateway_config,
            ..Self::default()
        })
    }

    #[cfg(test)]
    #[must_use]
    fn with_openai_models_url(mut self, url: &str) -> Self {
        self.openai_models_url = url.to_string();
        self
    }

    #[cfg(test)]
    #[must_use]
    fn with_openai_api_key(mut self, key: &str) -> Self {
        self.openai_api_key = Some(Secret::new(key.to_string()));
        self
    }
}

fn backend_client_from_env() -> gateway_backend_codex::client::CodexBackendClient {
    let client = gateway_backend_codex::client::CodexBackendClient::default();
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

async fn auth_refresh() -> axum::response::Response {
    let manager = gateway_auth_codex::CodexAuthManager::default();
    match manager.refresh_and_persist_default_path().await {
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
    let Some(api_key) = state.openai_api_key else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": {
                    "type": "config_error",
                    "message": "OPENAI_API_KEY is not set (env or ~/.gateway/auth.json); required to serve /v1/models from api.openai.com"
                }
            })),
        )
            .into_response();
    };

    let http = reqwest::Client::new();
    let res = http
        .get(&state.openai_models_url)
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {}", api_key.expose()),
        )
        .send()
        .await;

    let Ok(res) = res else {
        return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "error": { "type": "upstream_error", "message": "failed to reach api.openai.com /v1/models" }
                })),
            )
                .into_response();
    };

    if !res.status().is_success() {
        return (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({
                "error": { "type": "upstream_error", "message": format!("upstream returned {}", res.status()) }
            })),
        )
            .into_response();
    }

    let json: serde_json::Value = match res.json().await {
        Ok(v) => v,
        Err(_) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "error": { "type": "upstream_error", "message": "invalid JSON from upstream /v1/models" }
                })),
            )
                .into_response();
        }
    };

    let ids_iter = json["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.get("id").and_then(|id| id.as_str()).map(str::to_string));

    let mut ids: Vec<String> = ids_iter.collect();
    ids.sort();
    ids.dedup();

    let data: Vec<serde_json::Value> = ids
        .iter()
        .map(|id| serde_json::json!({ "id": id, "type": "model" }))
        .collect();

    Json(serde_json::json!({ "data": data })).into_response()
}

async fn v1_messages(
    State(state): State<AppState>,
    request_id: Option<axum::extract::Extension<RequestId>>,
    req: Request,
) -> axum::response::Response {
    let body = match read_request_body(req).await {
        Ok(b) => b,
        Err(resp) => return resp,
    };

    let req: AnthropicMessagesRequest = match deserialize_with_path(&body) {
        Ok(v) => v,
        Err(err) => return bad_request(&err),
    };

    log_ignored_stop_sequences(&req.stop_sequences, request_id.as_ref());
    log_ignored_request_controls(&req, request_id.as_ref());
    validate_tool_results(&state, &req);

    if req.stream {
        return stream_messages(state, request_id, req)
            .await
            .into_response();
    }

    let resolution = resolve_and_log_model(
        &state.gateway_config,
        &req.model,
        request_id.as_ref(),
        "resolved model for /v1/messages",
    );
    let tool_context = build_tool_translation_context(&state, &req);
    let translated = match translate_request_with_context(&req, &tool_context) {
        Ok(t) => t,
        Err(err) => return bad_request(&err),
    };

    let creds = match load_codex_credentials(auth_path_override(&state)) {
        Ok(c) => c,
        Err(err) => return auth_error(&err),
    };
    let backend_req = build_backend_request(&state.gateway_config, &resolution, translated, creds);
    let decoded = match run_backend_unary(&state, backend_req).await {
        Ok(d) => d,
        Err(resp) => return resp,
    };

    let usage = decoded.token_usage.map_or(
        serde_json::json!({ "input_tokens": 0, "output_tokens": 0 }),
        |u| serde_json::json!({ "input_tokens": u.input_tokens, "output_tokens": u.output_tokens }),
    );

    let response = if decoded.tool_calls.is_empty() {
        serde_json::json!({
        "id": format!("msg_{}", Uuid::new_v4()),
        "type": "message",
        "role": "assistant",
        "model": req.model,
        "content": [{ "type": "text", "text": decoded.final_text }],
        "stop_reason": "end_turn",
        "stop_sequence": null,
        "usage": usage
        })
    } else {
        let request_id_str = request_id
            .as_ref()
            .map(|axum::extract::Extension(r)| r.0.as_str());
        let mut content = Vec::new();
        if !decoded.final_text.is_empty() {
            content.push(serde_json::json!({ "type": "text", "text": decoded.final_text }));
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
    };

    let mut http_res = Json(response).into_response();
    http_res.extensions_mut().insert(resolution);
    http_res
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
        store: false,
        stream: true,
        include: translated.include,
        service_tier: service_tier_for_config(config),
        client_metadata: translated.client_metadata,
    }
}

async fn run_backend_unary(
    state: &AppState,
    backend_req: gateway_backend_codex::types::CodexBackendRequest,
) -> Result<gateway_backend_codex::types::CodexUnaryDecoded, axum::response::Response> {
    let res = state
        .backend
        .send_streaming_with_refresh_retry(&state.auth, backend_req)
        .await
        .map_err(|err| match err {
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
        })?;

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

    Ok(decoded)
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

async fn stream_messages(
    state: AppState,
    request_id: Option<axum::extract::Extension<RequestId>>,
    req: AnthropicMessagesRequest,
) -> Sse<futures_util::stream::BoxStream<'static, Result<Event, std::convert::Infallible>>> {
    let resolution = resolve_and_log_model(
        &state.gateway_config,
        &req.model,
        request_id.as_ref(),
        "resolved model for /v1/messages (streaming)",
    );
    let tool_context = build_tool_translation_context(&state, &req);
    let translated = match translate_request_with_context(&req, &tool_context) {
        Ok(t) => t,
        Err(err) => return sse_error("invalid_request_error", &err),
    };
    let creds = match load_codex_credentials(auth_path_override(&state)) {
        Ok(c) => c,
        Err(err) => return sse_auth_error(&err),
    };
    let request_to_backend =
        build_backend_request(&state.gateway_config, &resolution, translated, creds);

    let backend_response = state
        .backend
        .send_streaming_with_refresh_retry(&state.auth, request_to_backend)
        .await;

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

    let tail: futures_util::stream::BoxStream<'static, Result<Event, std::convert::Infallible>> =
        match backend_response {
            Ok(res) => backend_sse_to_anthropic_events(res, tool_calls, rid_str),
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

fn backend_sse_to_anthropic_events(
    res: reqwest::Response,
    tool_calls: ToolCallStore,
    request_id: Option<String>,
) -> futures_util::stream::BoxStream<'static, Result<Event, std::convert::Infallible>> {
    let backend_events = res.bytes_stream().eventsource();
    let state = Arc::new(Mutex::new(
        crate::sse_bridge::StreamState::new_with_text_block0_started(),
    ));
    let tool_calls = Arc::new(tool_calls);
    let request_id = Arc::new(request_id);

    backend_events
        .filter_map({
            let state = Arc::clone(&state);
            let tool_calls = Arc::clone(&tool_calls);
            let request_id = Arc::clone(&request_id);
            move |item| {
                let state = Arc::clone(&state);
                let tool_calls = Arc::clone(&tool_calls);
                let request_id = Arc::clone(&request_id);
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

                    crate::sse_bridge::map_backend_event(
                        &mut st,
                        evt.event.as_str(),
                        data,
                        gateway_backend_codex::output_text::extract_text_from_data,
                        &tool_calls,
                        request_id.as_ref().as_deref(),
                    )
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
    use super::build_tool_translation_context;
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
    use gateway_state::ToolCallStore;
    use tower::ServiceExt as _;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

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
        if std::env::var("RUN_WIREMOCK").ok().as_deref() != Some("1") {
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

    #[tokio::test]
    async fn v1_messages_unary_emits_tool_use_block_from_backend() {
        if std::env::var("RUN_WIREMOCK").ok().as_deref() != Some("1") {
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
        if std::env::var("RUN_WIREMOCK").ok().as_deref() != Some("1") {
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
        if std::env::var("RUN_WIREMOCK").ok().as_deref() != Some("1") {
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
}

#[cfg(test)]
mod models_api_tests {
    use super::{AppState, router};
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use gateway_core::DEFAULT_BACKEND_MODEL;
    use tower::ServiceExt as _;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn v1_models_proxies_openai_models_list() {
        if std::env::var("RUN_WIREMOCK").ok().as_deref() != Some("1") {
            return;
        }

        let mock = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .and(header("authorization", "Bearer test-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "object": "list",
                "data": [{"id": DEFAULT_BACKEND_MODEL}]
            })))
            .mount(&mock)
            .await;

        let state = AppState::default()
            .with_openai_models_url(&format!("{}/v1/models", mock.uri()))
            .with_openai_api_key("test-key");

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

        assert_eq!(ids, vec![DEFAULT_BACKEND_MODEL.to_string()]);
    }
}
