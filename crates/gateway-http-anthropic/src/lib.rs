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
use axum::routing::{get, post};
use gateway_core::RequestId;
use gateway_core::Secret;
use gateway_core::model_map::resolve_model;
use gateway_core::model_map::{ModelMap, default_model_map_path, load_model_map};
use tracing::info;
use uuid::Uuid;

#[derive(Clone, Default)]
pub struct AppState {
    auth: gateway_auth_codex::CodexAuthManager,
    backend: gateway_backend_codex::client::CodexBackendClient,
    openai_models_url: String,
    openai_api_key: Option<Secret<String>>,
}

impl AppState {
    #[must_use]
    pub fn from_env() -> Self {
        Self {
            openai_models_url: "https://api.openai.com/v1/models".to_string(),
            openai_api_key: std::env::var("OPENAI_API_KEY").ok().map(Secret::new),
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
            "source": "codex_auth_json",
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
            "source": "codex_auth_json",
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
                    "message": "OPENAI_API_KEY is not set; required to serve /v1/models from api.openai.com"
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

#[derive(Debug, serde::Deserialize)]
struct AnthropicMessagesRequest {
    model: String,
    messages: Vec<AnthropicMessage>,
    #[serde(default)]
    stream: bool,
}

#[derive(Debug, serde::Deserialize)]
struct AnthropicMessage {
    role: String,
    content: AnthropicContent,
}

#[derive(Debug, serde::Deserialize)]
#[serde(untagged)]
enum AnthropicContent {
    Text(String),
    Blocks(Vec<AnthropicContentBlock>),
}

#[derive(Debug, serde::Deserialize)]
struct AnthropicContentBlock {
    #[serde(rename = "type")]
    block_type: String,
    #[serde(default)]
    text: Option<String>,
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

    if req.stream {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": { "type": "invalid_request_error", "message": "stream=true is not supported yet" }
            })),
        )
            .into_response();
    }

    let map = load_model_map(&default_model_map_path()).unwrap_or(ModelMap {
        default_backend_model: "gpt-5.2".to_string(),
        aliases: std::collections::BTreeMap::new(),
    });
    let resolution = resolve_model(&map, &req.model);
    if let Some(axum::extract::Extension(rid)) = &request_id {
        info!(
            request_id = %rid.0,
            requested_model = %resolution.requested,
            selected_backend_model = %resolution.selected_backend_model,
            selection_reason = %resolution.selection_reason,
            "resolved model for /v1/messages"
        );
    }

    let Some(input_text) = extract_user_text(&req.messages) else {
        return bad_request("no user text content found");
    };

    let creds = match gateway_auth_codex::load_credentials_default_path() {
        Ok(c) => c,
        Err(err) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": { "type": "auth_error", "message": format!("{err}") }
                })),
            )
                .into_response();
        }
    };

    let backend_req = gateway_backend_codex::types::CodexBackendRequest {
        access_token: creds.access_token,
        account_id: creds.account_id,
        model: resolution.selected_backend_model.clone(),
        input_text,
    };

    let res = state
        .backend
        .send_streaming_with_refresh_retry(&state.auth, backend_req)
        .await;

    let final_text = match res {
        Ok(r) => {
            let stream = r.bytes_stream();
            let decoded = gateway_backend_codex::sse_unary::read_sse_to_completion(stream).await;
            match decoded {
                Ok(d) => d.final_text,
                Err(err) => {
                    return (
                        StatusCode::BAD_GATEWAY,
                        Json(serde_json::json!({
                            "error": { "type": "backend_error", "message": format!("{err}") }
                        })),
                    )
                        .into_response();
                }
            }
        }
        Err(err) => {
            return (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({
                    "error": { "type": "backend_error", "message": format!("{err}") }
                })),
            )
                .into_response();
        }
    };

    let response = serde_json::json!({
        "id": format!("msg_{}", Uuid::new_v4()),
        "type": "message",
        "role": "assistant",
        "model": req.model,
        "content": [{ "type": "text", "text": final_text }],
        "stop_reason": "end_turn",
        "stop_sequence": null,
        "usage": { "input_tokens": 0, "output_tokens": 0 }
    });

    let mut http_res = Json(response).into_response();
    http_res.extensions_mut().insert(resolution);
    http_res
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

fn extract_user_text(messages: &[AnthropicMessage]) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    for msg in messages {
        if msg.role != "user" {
            continue;
        }
        match &msg.content {
            AnthropicContent::Text(t) => {
                if !t.trim().is_empty() {
                    parts.push(t.clone());
                }
            }
            AnthropicContent::Blocks(blocks) => {
                for b in blocks {
                    if (b.block_type == "text" || b.block_type == "input_text")
                        && let Some(t) = &b.text
                        && !t.trim().is_empty()
                    {
                        parts.push(t.clone());
                    }
                }
            }
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}

#[cfg(test)]
mod messages_tests {
    use super::{AnthropicContent, AnthropicMessage, extract_user_text};
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt as _;

    #[test]
    fn user_text_extraction_supports_string_and_blocks() {
        let messages = vec![
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
                content: AnthropicContent::Blocks(vec![super::AnthropicContentBlock {
                    block_type: "text".to_string(),
                    text: Some("world".to_string()),
                }]),
            },
        ];

        assert_eq!(
            extract_user_text(&messages).as_deref(),
            Some("hello\nworld")
        );
    }

    #[tokio::test]
    async fn v1_messages_rejects_stream_true_without_backend_call() {
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

        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(
            json["error"]["message"]
                .as_str()
                .unwrap()
                .contains("stream=true")
        );
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
