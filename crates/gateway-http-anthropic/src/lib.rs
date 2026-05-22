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
use gateway_core::RequestId;
use gateway_core::Secret;
use gateway_core::model_map::resolve_model;
use gateway_core::model_map::{ModelMap, ModelResolution, default_model_map_path, load_model_map};
use std::sync::{Arc, Mutex};
use tracing::info;
use uuid::Uuid;

mod translate;
mod types;

use crate::translate::translate_request;
use crate::types::AnthropicMessagesRequest;
use gateway_state::ToolCallStore;

#[derive(Clone, Default)]
pub struct AppState {
    auth: gateway_auth_codex::CodexAuthManager,
    backend: gateway_backend_codex::client::CodexBackendClient,
    openai_models_url: String,
    openai_api_key: Option<Secret<String>>,
    tool_calls: ToolCallStore,
}

impl AppState {
    #[must_use]
    pub fn from_env() -> Self {
        let openai_api_key = std::env::var("OPENAI_API_KEY")
            .ok()
            .map(Secret::new)
            .or_else(|| {
                gateway_auth_codex::load_openai_api_key_default_path()
                    .ok()
                    .flatten()
            });
        Self {
            openai_models_url: "https://api.openai.com/v1/models".to_string(),
            openai_api_key,
            ..Self::default()
        }
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

async fn auth_status() -> impl IntoResponse {
    match gateway_auth_codex::load_codex_auth_default_path() {
        Ok(snap) => Json(serde_json::json!({
            "logged_in": snap.has_access_token && snap.has_refresh_token,
            "account_id": snap.account_id,
            "expires_at_unix_seconds": snap.expires_at_unix_seconds,
            "source": "gateway_auth_json",
        })),
        Err(err) => Json(serde_json::json!({
            "logged_in": false,
            "account_id": null,
            "expires_at_unix_seconds": null,
            "source": "error",
            "error_type": format!("{err}"),
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
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "ok": false,
                "error_type": format!("{err}"),
            })),
        )
            .into_response(),
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
    if let Err(message) = validate_tool_results(&state, &req) {
        return bad_request(&message);
    }

    if req.stream {
        return stream_messages(state, request_id, req)
            .await
            .into_response();
    }

    let resolution = resolve_and_log_model(
        &req.model,
        request_id.as_ref(),
        "resolved model for /v1/messages",
    );
    let translated = match translate_request(&req) {
        Ok(t) => t,
        Err(err) => return bad_request(&err),
    };

    let creds = match load_codex_credentials() {
        Ok(c) => c,
        Err(resp) => return *resp,
    };
    let backend_req = build_backend_request(&resolution, translated, creds);
    let decoded = match run_backend_unary(&state, backend_req).await {
        Ok(d) => d,
        Err(resp) => return resp,
    };

    let response = if let Some(tool_call) = decoded.tool_call {
        let _ = state.tool_calls.record_tool_call(
            &tool_call.call_id,
            &tool_call.name,
            request_id
                .as_ref()
                .map(|axum::extract::Extension(r)| r.0.as_str()),
        );
        let input_value: serde_json::Value =
            serde_json::from_str(&tool_call.arguments).unwrap_or(serde_json::json!({}));
        serde_json::json!({
            "id": format!("msg_{}", Uuid::new_v4()),
            "type": "message",
            "role": "assistant",
            "model": req.model,
            "content": [{
                "type": "tool_use",
                "id": tool_call.call_id,
                "name": tool_call.name,
                "input": input_value
            }],
            "stop_reason": "tool_use",
            "stop_sequence": null,
            "usage": { "input_tokens": 0, "output_tokens": 0 }
        })
    } else {
        serde_json::json!({
        "id": format!("msg_{}", Uuid::new_v4()),
        "type": "message",
        "role": "assistant",
        "model": req.model,
        "content": [{ "type": "text", "text": decoded.final_text }],
        "stop_reason": "end_turn",
        "stop_sequence": null,
        "usage": { "input_tokens": 0, "output_tokens": 0 }
        })
    };

    let mut http_res = Json(response).into_response();
    http_res.extensions_mut().insert(resolution);
    http_res
}

fn resolve_and_log_model(
    model: &str,
    request_id: Option<&axum::extract::Extension<RequestId>>,
    msg: &str,
) -> ModelResolution {
    let map = load_model_map(&default_model_map_path()).unwrap_or(ModelMap {
        default_backend_model: "gpt-5.2".to_string(),
        aliases: std::collections::BTreeMap::new(),
    });
    let resolution = resolve_model(&map, model);
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

fn load_codex_credentials()
-> Result<gateway_auth_codex::CodexCredentials, Box<axum::response::Response>> {
    gateway_auth_codex::load_credentials_default_path().map_err(|err| {
        Box::new(
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": { "type": "auth_error", "message": format!("{err}") }
                })),
            )
                .into_response(),
        )
    })
}

fn build_backend_request(
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
        .map_err(|err| {
            (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "error": { "type": "backend_error", "message": format!("{err}") }
                })),
            )
                .into_response()
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
    if req.temperature.is_some() || req.top_p.is_some() || req.top_k.is_some() {
        if let Some(rid) = &rid {
            tracing::warn!(
                request_id = %rid,
                "ignoring Anthropic sampling controls (temperature/top_p/top_k) (not forwarded yet)"
            );
        } else {
            tracing::warn!(
                "ignoring Anthropic sampling controls (temperature/top_p/top_k) (not forwarded yet)"
            );
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

    // `output_config` is partially supported (json_schema format), but other knobs may be ignored.
    if let Some(cfg) = req.output_config.as_ref()
        && cfg.effort.is_some()
    {
        if let Some(rid) = &rid {
            tracing::warn!(
                request_id = %rid,
                "ignoring Anthropic output_config.effort (not forwarded yet)"
            );
        } else {
            tracing::warn!("ignoring Anthropic output_config.effort (not forwarded yet)");
        }
    }
}

fn validate_tool_results(state: &AppState, req: &AnthropicMessagesRequest) -> Result<(), String> {
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
                return Err(format!(
                    "unknown tool_use_id: {tool_use_id} (no prior tool_use emitted by this gateway)"
                ));
            }
        }
    }
    Ok(())
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
                    "usage": { "input_tokens": 0, "output_tokens": 0 }
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
        &req.model,
        request_id.as_ref(),
        "resolved model for /v1/messages (streaming)",
    );
    let translated = match translate_request(&req) {
        Ok(t) => t,
        Err(err) => return sse_error("invalid_request_error", &err),
    };
    let creds = match gateway_auth_codex::load_credentials_default_path() {
        Ok(c) => c,
        Err(err) => return sse_error("auth_error", &format!("{err}")),
    };
    let request_to_backend = build_backend_request(&resolution, translated, creds);

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

    let tail: futures_util::stream::BoxStream<'static, Result<Event, std::convert::Infallible>> =
        match backend_response {
            Ok(res) => backend_sse_to_anthropic_events(res),
            Err(err) => {
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

#[allow(clippy::too_many_lines)]
fn backend_sse_to_anthropic_events(
    res: reqwest::Response,
) -> futures_util::stream::BoxStream<'static, Result<Event, std::convert::Infallible>> {
    #[derive(Default)]
    struct StreamState {
        tool_block_started: bool,
        tool_call_id: Option<String>,
        tool_call_name: Option<String>,
        completed: bool,
    }

    fn tool_block_start(st: &StreamState) -> Event {
        let payload = serde_json::json!({
            "type": "content_block_start",
            "index": 1,
            "content_block": {
                "type": "tool_use",
                "id": st.tool_call_id.clone().unwrap_or_default(),
                "name": st.tool_call_name.clone().unwrap_or_default(),
                "input": {}
            }
        })
        .to_string();
        Event::default().event("content_block_start").data(payload)
    }

    fn tool_args_delta(delta: &str) -> Event {
        let payload = serde_json::json!({
            "type": "content_block_delta",
            "index": 1,
            "delta": { "type": "input_json_delta", "partial_json": delta }
        })
        .to_string();
        Event::default().event("content_block_delta").data(payload)
    }

    fn text_delta(text: &str) -> Event {
        let payload = serde_json::json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": { "type": "text_delta", "text": text }
        })
        .to_string();
        Event::default().event("content_block_delta").data(payload)
    }

    fn finalize_message(st: &mut StreamState) -> Vec<Event> {
        st.completed = true;
        let mut out = Vec::new();
        out.push(
            Event::default()
                .event("content_block_stop")
                .data(serde_json::json!({"type":"content_block_stop","index":0}).to_string()),
        );
        if st.tool_block_started {
            out.push(
                Event::default()
                    .event("content_block_stop")
                    .data(serde_json::json!({"type":"content_block_stop","index":1}).to_string()),
            );
            out.push(
                Event::default().event("message_delta").data(
                    serde_json::json!({
                        "type":"message_delta",
                        "delta":{"stop_reason":"tool_use","stop_sequence":null},
                        "usage":{"output_tokens":0}
                    })
                    .to_string(),
                ),
            );
        } else {
            out.push(
                Event::default().event("message_delta").data(
                    serde_json::json!({
                        "type":"message_delta",
                        "delta":{"stop_reason":"end_turn","stop_sequence":null},
                        "usage":{"output_tokens":0}
                    })
                    .to_string(),
                ),
            );
        }
        out.push(
            Event::default()
                .event("message_stop")
                .data(serde_json::json!({"type":"message_stop"}).to_string()),
        );
        out
    }

    fn map_backend_event(st: &mut StreamState, event_name: &str, data: &str) -> Option<Vec<Event>> {
        match event_name {
            "response.output_text.delta" => {
                let text = extract_stream_delta_text(data)?;
                Some(vec![text_delta(&text)])
            }
            "response.output_item.added" | "response.output_item.done" => {
                let v = serde_json::from_str::<serde_json::Value>(data).ok()?;
                let item = v.get("item")?;
                if item.get("type").and_then(|v| v.as_str()) != Some("function_call") {
                    return None;
                }
                if st.tool_block_started {
                    return None;
                }
                st.tool_block_started = true;
                st.tool_call_id = item
                    .get("call_id")
                    .and_then(|s| s.as_str())
                    .map(str::to_string);
                st.tool_call_name = item
                    .get("name")
                    .and_then(|s| s.as_str())
                    .map(str::to_string);
                Some(vec![tool_block_start(st)])
            }
            "response.function_call_arguments.delta" => {
                if !st.tool_block_started {
                    return None;
                }
                let v = serde_json::from_str::<serde_json::Value>(data).ok()?;
                let delta = v.get("delta").and_then(|v| v.as_str())?;
                Some(vec![tool_args_delta(delta)])
            }
            "response.completed" => Some(finalize_message(st)),
            _ => None,
        }
    }

    let backend_events = res.bytes_stream().eventsource();
    let state = Arc::new(Mutex::new(StreamState::default()));

    backend_events
        .filter_map({
            let state = Arc::clone(&state);
            move |item| {
                let state = Arc::clone(&state);
                async move {
                    let mut st = state.lock().unwrap();
                    if st.completed {
                        return None;
                    }

                    let evt = match item {
                        Ok(e) => e,
                        Err(e) => {
                            st.completed = true;
                            let payload = serde_json::json!({
                                "type": "error",
                                "error": { "type": "upstream_error", "message": format!("{e}") }
                            })
                            .to_string();
                            return Some(vec![Event::default().event("error").data(payload)]);
                        }
                    };

                    let data = evt.data.trim();
                    if data.is_empty() || data == "[DONE]" {
                        return None;
                    }

                    map_backend_event(&mut st, evt.event.as_str(), data)
                }
            }
        })
        .flat_map(|events| {
            futures_util::stream::iter(events.into_iter().map(Ok::<_, std::convert::Infallible>))
        })
        .boxed()
}

fn extract_stream_delta_text(data: &str) -> Option<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
        return Some(data.to_string());
    };

    let mut last = None;
    extract_last_text_from_value(&value, &mut last);
    last
}

fn extract_last_text_from_value(value: &serde_json::Value, last: &mut Option<String>) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::String(text)) = map.get("text") {
                *last = Some(text.clone());
            }
            if let Some(serde_json::Value::String(delta)) = map.get("delta") {
                *last = Some(delta.clone());
            }
            if let Some(content) = map.get("content") {
                extract_last_text_from_value(content, last);
            }
            for (k, child) in map {
                if k == "text" || k == "delta" || k == "content" {
                    continue;
                }
                extract_last_text_from_value(child, last);
            }
        }
        serde_json::Value::Array(items) => {
            for child in items {
                extract_last_text_from_value(child, last);
            }
        }
        serde_json::Value::String(_)
        | serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_) => {}
    }
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

// NOTE: Request translation lives in `translate.rs`. Keep handler code free of ad-hoc extraction
// logic so we can maintain full-history fidelity.

#[cfg(test)]
mod messages_tests {
    use super::translate::translate_request;
    use super::types::{
        AnthropicContent, AnthropicContentBlock, AnthropicMessage, AnthropicMessagesRequest,
    };
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt as _;

    #[test]
    fn translation_accepts_string_and_blocks_text() {
        let req = AnthropicMessagesRequest {
            model: "gpt-5.2".to_string(),
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

    #[tokio::test]
    async fn v1_messages_supports_stream_true() {
        let app = super::router(super::AppState::default());
        let req_body = serde_json::json!({
            "model": "gpt-5.2",
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
    }
}

#[cfg(test)]
mod models_api_tests {
    use super::{AppState, router};
    use axum::body::{Body, to_bytes};
    use axum::http::Request;
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
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                r#"{"object":"list","data":[{"id":"gpt-5.2"},{"id":"gpt-5.2-codex"}]}"#,
                "application/json",
            ))
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

        assert_eq!(
            ids,
            vec!["gpt-5.2".to_string(), "gpt-5.2-codex".to_string()]
        );
    }
}
