#![forbid(unsafe_code)]

use crate::types::{CodexBackendRequest, CodexBackendResponse};
use gateway_auth_codex::CodexAuthManager;
use reqwest::Response;
use std::time::Duration;
use url::Url;

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("request failed")]
    RequestFailed,
    #[error("unexpected response status {0}")]
    UnexpectedStatus(u16),
    #[error("unexpected response status {status}: {body}")]
    UnexpectedStatusWithBody { status: u16, body: String },
}

#[derive(Clone)]
pub struct CodexBackendClient {
    http: reqwest::Client,
    base_url: String,
}

impl Default for CodexBackendClient {
    fn default() -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: "https://chatgpt.com".to_string(),
        }
    }
}

impl CodexBackendClient {
    #[must_use]
    pub fn with_base_url(mut self, base_url: &Url) -> Self {
        self.base_url = base_url.to_string();
        self
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
        let body_text = res.text().await.map_err(|_| BackendError::RequestFailed)?;

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

        let res = self
            .http
            .post(url)
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {}", req.access_token.expose()),
            )
            .header("chatgpt-account-id", &req.account_id)
            .header("OpenAI-Beta", "responses=experimental")
            .header("originator", "codex_cli_rs")
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .timeout(Duration::from_mins(1))
            .json(&body)
            .send()
            .await
            .map_err(|_| BackendError::RequestFailed)?;

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
                let refreshed = auth
                    .refresh_and_persist_default_path()
                    .await
                    .map_err(|_| BackendError::RequestFailed)?;

                // Re-load the access token after refresh.
                req.access_token = gateway_auth_codex::load_access_token_default_path()
                    .map_err(|_| BackendError::RequestFailed)?;
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
                let refreshed = auth
                    .refresh_and_persist_default_path()
                    .await
                    .map_err(|_| BackendError::RequestFailed)?;

                req.access_token = gateway_auth_codex::load_access_token_default_path()
                    .map_err(|_| BackendError::RequestFailed)?;
                req.account_id = refreshed.account_id;

                self.send_streaming(&req).await
            }
            Err(e) => Err(e),
        }
    }
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

fn build_request_body(req: &CodexBackendRequest) -> serde_json::Value {
    // Minimal body that resembles Responses API shape. Keep it flexible until Day 13.
    serde_json::json!({
        "model": req.model,
        "instructions": req.instructions,
        // The ChatGPT Codex backend requires `store=false` (Codex CLI sets this) to avoid persisting
        // requests. If omitted, the backend defaults to storing and rejects the request.
        "store": false,
        // We always consume the backend response as `text/event-stream`, so request streaming.
        "stream": true,
        "input": [
            {
                "role": "user",
                "content": [
                    { "type": "input_text", "text": req.input_text }
                ]
            }
        ]
    })
}
