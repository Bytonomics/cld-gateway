#![forbid(unsafe_code)]

use crate::types::{CodexBackendRequest, CodexBackendResponse};
use gateway_auth_codex::CodexAuthManager;
use std::time::Duration;
use url::Url;

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("request failed")]
    RequestFailed,
    #[error("unexpected response status {0}")]
    UnexpectedStatus(u16),
}

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
        let body_text = res.text().await.map_err(|_| BackendError::RequestFailed)?;

        if status == 401 {
            return Err(BackendError::UnexpectedStatus(status));
        }

        if status >= 300 {
            return Err(BackendError::UnexpectedStatus(status));
        }

        Ok(CodexBackendResponse {
            status,
            body: body_text,
        })
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
            Err(BackendError::UnexpectedStatus(401)) => {
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
}

fn build_request_body(req: &CodexBackendRequest) -> serde_json::Value {
    // Minimal body that resembles Responses API shape. Keep it flexible until Day 13.
    serde_json::json!({
        "model": req.model,
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
