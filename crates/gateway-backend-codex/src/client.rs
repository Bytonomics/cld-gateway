#![forbid(unsafe_code)]

use crate::types::{
    CodexBackendEvent, CodexBackendEventStream, CodexBackendRequest, CodexBackendResponse,
};
use crate::websocket_transport::WebSocketSessionPool;
pub use crate::websocket_transport::{
    WebSocketChainId, WebSocketErrorMatcher, WebSocketErrorVariant, WebSocketRetryPolicy,
    WebSocketSessionKey,
};
use eventsource_stream::Eventsource as _;
use futures_util::StreamExt as _;
use gateway_auth_codex::CodexAuthManager;
use gateway_core::format_error_chain;
use gateway_net::{GatewayHttpClient, NetworkPolicyError};
use reqwest::Response;
use std::time::Duration;
use url::Url;

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("request failed during {stage}: {message}")]
    RequestFailed {
        stage: &'static str,
        message: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("authentication failed during {stage}: {message}")]
    AuthFailed {
        stage: &'static str,
        message: String,
    },
    #[error("outbound request blocked during {stage}: {message}")]
    NetworkPolicy {
        stage: &'static str,
        message: String,
    },
    #[error("websocket failed during {stage}: {message}")]
    WebSocket {
        stage: &'static str,
        message: String,
    },
    #[error("unexpected response status {0}")]
    UnexpectedStatus(u16),
    #[error("unexpected response status {status}: {body}")]
    UnexpectedStatusWithBody { status: u16, body: String },
}

#[derive(Clone)]
pub struct CodexBackendClient {
    http: GatewayHttpClient,
    base_url: String,
    request_timeout: Option<Duration>,
    websocket_sessions: WebSocketSessionPool,
}

impl Default for CodexBackendClient {
    fn default() -> Self {
        Self {
            http: GatewayHttpClient::default(),
            base_url: "https://chatgpt.com".to_string(),
            request_timeout: None,
            websocket_sessions: WebSocketSessionPool::default(),
        }
    }
}

impl CodexBackendClient {
    #[must_use]
    pub fn with_base_url(mut self, base_url: &Url) -> Self {
        self.base_url = base_url.to_string();
        self
    }

    #[must_use]
    pub fn with_http_client(mut self, http: GatewayHttpClient) -> Self {
        self.http = http;
        self
    }

    #[must_use]
    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = Some(timeout);
        self
    }

    #[must_use]
    pub fn request_timeout(&self) -> Option<Duration> {
        self.request_timeout
    }

    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    #[must_use]
    pub fn http_client(&self) -> &GatewayHttpClient {
        &self.http
    }

    /// Sends a request to the Codex backend.
    ///
    /// # Errors
    ///
    /// Returns an error on transport failures or non-success HTTP status.
    pub async fn send(
        &self,
        req: &CodexBackendRequest,
    ) -> Result<CodexBackendResponse, BackendError> {
        let res = self.send_streaming(req).await?;
        let status = res.status().as_u16();
        let body_text = res
            .text()
            .await
            .map_err(|source| request_failed("read response body", source))?;

        Ok(CodexBackendResponse {
            status,
            body: body_text,
        })
    }

    /// Sends a request to the Codex backend and returns the raw streaming response.
    ///
    /// This is used for consuming `text/event-stream` bodies (Day 11/13).
    ///
    /// # Errors
    ///
    /// Returns an error on transport failures or non-success HTTP status.
    pub async fn send_streaming(
        &self,
        req: &CodexBackendRequest,
    ) -> Result<Response, BackendError> {
        let url = format!(
            "{}/backend-api/codex/responses",
            self.base_url.trim_end_matches('/')
        );

        let body = build_request_body(req);
        let authorization = format!("Bearer {}", req.access_token.expose());

        let mut builder = self
            .http
            .post(&url)
            .map_err(|source| network_policy_failed("prepare request", &source))?
            .header("Authorization", &authorization)
            .header("chatgpt-account-id", &req.account_id)
            .header("OpenAI-Beta", "responses=experimental")
            .header("originator", "codex_cli_rs")
            .header("Accept", "text/event-stream");

        if let Some(timeout) = self.request_timeout {
            builder = builder.timeout(timeout);
        }

        let res = builder
            .json(&body)
            .execute()
            .await
            .map_err(|source| request_failed("send request", source))?;

        let status = res.status().as_u16();
        if status >= 300 {
            let body = res
                .text()
                .await
                .ok()
                .map(|s| truncate_error_body(&s))
                .filter(|s| !s.trim().is_empty());
            if let Some(body) = body {
                return Err(BackendError::UnexpectedStatusWithBody { status, body });
            }
            return Err(BackendError::UnexpectedStatus(status));
        }

        Ok(res)
    }

    /// Day 8 contract: on 401, refresh once, retry once, then fail.
    ///
    /// # Errors
    ///
    /// Returns an error if the initial request fails, refresh fails, or retry fails.
    pub async fn send_with_refresh_retry(
        &self,
        auth: &CodexAuthManager,
        mut req: CodexBackendRequest,
    ) -> Result<CodexBackendResponse, BackendError> {
        match self.send(&req).await {
            Ok(r) => Ok(r),
            Err(
                BackendError::UnexpectedStatus(401)
                | BackendError::UnexpectedStatusWithBody { status: 401, .. },
            ) => {
                let refreshed = match auth.refresh_and_persist_default_path().await {
                    Ok(snapshot) => snapshot,
                    Err(err) => {
                        if err.is_permanent_refresh_failure() {
                            let _ = gateway_auth_codex::logout_with_revoke_default_path().await;
                        }
                        return Err(BackendError::AuthFailed {
                            stage: "refresh auth",
                            message: err.to_string(),
                        });
                    }
                };

                // Re-load the access token after refresh.
                req.access_token =
                    gateway_auth_codex::load_access_token_default_path().map_err(|err| {
                        BackendError::AuthFailed {
                            stage: "reload access token",
                            message: err.to_string(),
                        }
                    })?;
                req.account_id = refreshed.account_id;

                self.send(&req).await
            }
            Err(e) => Err(e),
        }
    }

    /// Day 8 contract (streaming): on 401, refresh once, retry once, then fail.
    ///
    /// # Errors
    ///
    /// Returns an error if the initial request fails, refresh fails, or retry fails.
    pub async fn send_streaming_with_refresh_retry(
        &self,
        auth: &CodexAuthManager,
        mut req: CodexBackendRequest,
    ) -> Result<Response, BackendError> {
        match self.send_streaming(&req).await {
            Ok(r) => Ok(r),
            Err(
                BackendError::UnexpectedStatus(401)
                | BackendError::UnexpectedStatusWithBody { status: 401, .. },
            ) => {
                let refreshed = match auth.refresh_and_persist_default_path().await {
                    Ok(snapshot) => snapshot,
                    Err(err) => {
                        if err.is_permanent_refresh_failure() {
                            let _ = gateway_auth_codex::logout_with_revoke_default_path().await;
                        }
                        return Err(BackendError::AuthFailed {
                            stage: "refresh auth",
                            message: err.to_string(),
                        });
                    }
                };

                req.access_token =
                    gateway_auth_codex::load_access_token_default_path().map_err(|err| {
                        BackendError::AuthFailed {
                            stage: "reload access token",
                            message: err.to_string(),
                        }
                    })?;
                req.account_id = refreshed.account_id;

                self.send_streaming(&req).await
            }
            Err(e) => Err(e),
        }
    }

    /// Converts the existing HTTP/SSE transport into backend events.
    #[must_use]
    pub fn response_to_event_stream(res: Response) -> CodexBackendEventStream {
        res.bytes_stream()
            .eventsource()
            .map(|item| {
                item.map(|event| CodexBackendEvent {
                    event: event.event,
                    data: event.data,
                })
                .map_err(|err| BackendError::WebSocket {
                    stage: "decode http event stream",
                    message: format!("{err}"),
                })
            })
            .boxed()
    }

    /// Sends an incremental request over the pooled Codex Responses WebSocket transport.
    ///
    /// # Errors
    /// Returns an error if opening the WebSocket fails before a stream can be created.
    /// Stale pooled sockets are recycled inside the transport before the first event is exposed.
    pub async fn send_pooled_websocket_event_stream(
        &self,
        auth: &CodexAuthManager,
        session_key: WebSocketSessionKey,
        req: CodexBackendRequest,
        policy: WebSocketRetryPolicy,
    ) -> Result<CodexBackendEventStream, BackendError> {
        self.websocket_sessions
            .send_event_stream(&self.base_url, auth, session_key, req, policy)
            .await
    }

    #[must_use]
    pub fn has_live_websocket_session(&self, session_key: &WebSocketSessionKey) -> bool {
        self.websocket_sessions.has_live_session(session_key)
    }

    #[must_use]
    pub fn live_websocket_chain_id(
        &self,
        session_key: &WebSocketSessionKey,
    ) -> Option<WebSocketChainId> {
        self.websocket_sessions.live_websocket_chain_id(session_key)
    }
}

fn request_failed(stage: &'static str, source: reqwest::Error) -> BackendError {
    let message = format_error_chain(&source);
    BackendError::RequestFailed {
        stage,
        message,
        source,
    }
}

fn network_policy_failed(stage: &'static str, source: &NetworkPolicyError) -> BackendError {
    BackendError::NetworkPolicy {
        stage,
        message: source.to_string(),
    }
}

pub(crate) fn websocket_failed<E>(stage: &'static str, source: E) -> BackendError
where
    E: std::fmt::Display,
{
    let message = source.to_string();
    if let Some(status) = websocket_status_from_error_message(&message) {
        return BackendError::UnexpectedStatus(status);
    }
    BackendError::WebSocket { stage, message }
}

pub(crate) fn websocket_message<E>(stage: &'static str, source: E) -> BackendError
where
    E: std::fmt::Display,
{
    BackendError::WebSocket {
        stage,
        message: source.to_string(),
    }
}

pub(crate) fn websocket_status_from_error_message(message: &str) -> Option<u16> {
    [401_u16, 403, 404, 409, 410, 422, 429, 500, 502, 503]
        .into_iter()
        .find(|status| message.contains(&status.to_string()))
}

fn truncate_error_body(body: &str) -> String {
    const MAX: usize = 8 * 1024;
    if body.len() <= MAX {
        body.to_string()
    } else {
        let mut s = body[..MAX].to_string();
        s.push_str("…(truncated)");
        s
    }
}

pub(crate) fn build_request_body(req: &CodexBackendRequest) -> serde_json::Value {
    // Body that matches Codex's "Responses-like" payload shape as closely as possible.
    let mut obj = serde_json::Map::new();
    obj.insert(
        "model".to_string(),
        serde_json::Value::String(req.model.clone()),
    );
    obj.insert(
        "instructions".to_string(),
        serde_json::Value::String(req.instructions.clone()),
    );
    obj.insert(
        "input".to_string(),
        serde_json::Value::Array(req.input.clone()),
    );
    obj.insert(
        "tools".to_string(),
        serde_json::Value::Array(req.tools.clone()),
    );
    obj.insert(
        "tool_choice".to_string(),
        serde_json::Value::String(req.tool_choice.clone()),
    );
    obj.insert(
        "parallel_tool_calls".to_string(),
        serde_json::Value::Bool(req.parallel_tool_calls),
    );
    if let Some(reasoning) = req.reasoning.clone() {
        obj.insert("reasoning".to_string(), reasoning);
    }
    if let Some(text) = req.text.clone() {
        obj.insert("text".to_string(), text);
    }
    obj.insert(
        "store".to_string(),
        serde_json::Value::Bool(crate::types::STORE_RESPONSES_FOR_CONTINUATION),
    );
    // Backend returns SSE when stream=true.
    obj.insert("stream".to_string(), serde_json::Value::Bool(req.stream));
    obj.insert(
        "include".to_string(),
        serde_json::Value::Array(
            req.include
                .iter()
                .cloned()
                .map(serde_json::Value::String)
                .collect(),
        ),
    );
    if let Some(service_tier) = req.service_tier.as_ref() {
        obj.insert(
            "service_tier".to_string(),
            serde_json::Value::String(service_tier.clone()),
        );
    }

    if let Some(meta) = req.client_metadata.as_ref() {
        let mut m = serde_json::Map::new();
        for (k, v) in meta {
            m.insert(k.clone(), serde_json::Value::String(v.clone()));
        }
        obj.insert("client_metadata".to_string(), serde_json::Value::Object(m));
    }

    serde_json::Value::Object(obj)
}

#[cfg(test)]
mod tests {
    use super::CodexBackendClient;
    use std::time::Duration;

    #[test]
    fn default_client_has_no_total_request_timeout() {
        assert_eq!(CodexBackendClient::default().request_timeout(), None);
    }

    #[test]
    fn request_timeout_can_be_configured() {
        let timeout = Duration::from_secs(123);
        let client = CodexBackendClient::default().with_request_timeout(timeout);
        assert_eq!(client.request_timeout(), Some(timeout));
    }
}
