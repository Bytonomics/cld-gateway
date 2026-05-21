// Crate: gateway-auth-codex
// Purpose: load/refresh Codex OAuth credentials from disk and provide safe auth snapshots.
// Allowed deps: gateway-core.
// Not allowed: axum/http routing, backend client code.

#![forbid(unsafe_code)]

mod auth_json;
mod jwt;
pub mod oauth;
pub mod paths;
mod persist;

use gateway_core::Secret;
use std::path::Path;
use url::Url;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexAuthSnapshot {
    pub account_id: String,
    pub has_access_token: bool,
    pub has_refresh_token: bool,
    pub expires_at_unix_seconds: Option<i64>,
}

#[derive(Debug, thiserror::Error)]
pub enum CodexAuthError {
    #[error("failed to read auth.json")]
    Io(#[from] std::io::Error),

    #[error("failed to parse auth.json")]
    Json(#[from] serde_json::Error),

    #[error("auth.json missing required field: {0}")]
    MissingField(&'static str),

    #[error("access token is not a JWT")]
    JwtMalformed,

    #[error("failed to decode JWT payload")]
    JwtDecode,

    #[error("failed to parse JWT claims JSON")]
    JwtClaimsJson,

    #[error("token refresh failed")]
    RefreshFailed,

    #[error("token endpoint returned unexpected response")]
    RefreshUnexpectedResponse,
}

/// Loads Codex auth from the provided `auth.json` path and returns a safe snapshot.
///
/// # Errors
///
/// Returns an error if the file cannot be read, parsed, or does not contain the required fields.
pub fn load_codex_auth(path: &Path) -> Result<CodexAuthSnapshot, CodexAuthError> {
    let bytes = std::fs::read(path)?;
    let auth: auth_json::AuthJson = serde_json::from_slice(&bytes)?;

    // `id_token` is part of Codex auth.json; we intentionally don't expose it in the snapshot, but
    // we do read it so it remains covered by parsing/fixtures and doesn't rot.
    let _id_token_present = auth.tokens.id_token.is_some();

    let access_token = auth
        .tokens
        .access_token
        .as_deref()
        .ok_or(CodexAuthError::MissingField("tokens.access_token"))?;
    let refresh_token_present = auth.tokens.refresh_token.is_some();

    let account_id = auth
        .tokens
        .account_id
        .ok_or(CodexAuthError::MissingField("tokens.account_id"))?;

    let exp = jwt::extract_exp_unverified(access_token)?;

    Ok(CodexAuthSnapshot {
        account_id,
        has_access_token: true,
        has_refresh_token: refresh_token_present,
        expires_at_unix_seconds: exp,
    })
}

/// Loads Codex auth from the default path (`$CODEX_HOME/auth.json` or `~/.codex/auth.json`).
///
/// # Errors
///
/// Returns an error if loading/parsing fails or required fields are missing.
pub fn load_codex_auth_default_path() -> Result<CodexAuthSnapshot, CodexAuthError> {
    load_codex_auth(&paths::default_auth_json_path())
}

/// Loads the current access token from the default auth path.
///
/// This is intentionally a minimal helper for early backend work; Day 8 introduces refresh/persistence.
///
/// # Errors
///
/// Returns an error if loading/parsing fails or required fields are missing.
pub fn load_access_token_default_path() -> Result<Secret<String>, CodexAuthError> {
    let bytes = std::fs::read(paths::default_auth_json_path())?;
    let auth: auth_json::AuthJson = serde_json::from_slice(&bytes)?;

    let access_token = auth
        .tokens
        .access_token
        .ok_or(CodexAuthError::MissingField("tokens.access_token"))?;

    Ok(Secret::new(access_token))
}

#[derive(Clone)]
pub struct CodexCredentials {
    pub access_token: Secret<String>,
    pub account_id: String,
}

/// Loads access token + account id from the default auth path in one IO/parse pass.
///
/// # Errors
///
/// Returns an error if loading/parsing fails or required fields are missing.
pub fn load_credentials_default_path() -> Result<CodexCredentials, CodexAuthError> {
    let bytes = std::fs::read(paths::default_auth_json_path())?;
    let auth: auth_json::AuthJson = serde_json::from_slice(&bytes)?;

    let access_token = auth
        .tokens
        .access_token
        .ok_or(CodexAuthError::MissingField("tokens.access_token"))?;

    let account_id = auth
        .tokens
        .account_id
        .ok_or(CodexAuthError::MissingField("tokens.account_id"))?;

    Ok(CodexCredentials {
        access_token: Secret::new(access_token),
        account_id,
    })
}

#[derive(Clone)]
pub struct CodexAuthManager {
    token_url: String,
    client_id: String,
    http: reqwest::Client,
}

impl Default for CodexAuthManager {
    fn default() -> Self {
        Self {
            // Per plan Day 8.
            token_url: "https://auth.openai.com/oauth/token".to_string(),
            client_id: "app_EMoamEEZ73f0CkXaXp7hrann".to_string(),
            http: reqwest::Client::new(),
        }
    }
}

impl CodexAuthManager {
    #[must_use]
    pub fn with_token_url(mut self, token_url: &Url) -> Self {
        self.token_url = token_url.to_string();
        self
    }

    /// Refreshes tokens using the `refresh_token` grant and persists updates atomically back to auth.json.
    ///
    /// # Errors
    ///
    /// Returns an error if loading/parsing auth.json fails, refresh fails, or persistence fails.
    pub async fn refresh_and_persist_default_path(
        &self,
    ) -> Result<CodexAuthSnapshot, CodexAuthError> {
        let path = paths::default_auth_json_path();
        self.refresh_and_persist(&path).await
    }

    /// Refreshes tokens using the `refresh_token` grant and persists updates atomically back to `path`.
    ///
    /// # Errors
    ///
    /// Returns an error if loading/parsing auth.json fails, refresh fails, or persistence fails.
    pub async fn refresh_and_persist(
        &self,
        path: &Path,
    ) -> Result<CodexAuthSnapshot, CodexAuthError> {
        let bytes = std::fs::read(path)?;
        let mut auth_value: serde_json::Value = serde_json::from_slice(&bytes)?;

        let refresh_token = auth_value
            .pointer("/tokens/refresh_token")
            .and_then(|v| v.as_str())
            .ok_or(CodexAuthError::MissingField("tokens.refresh_token"))?;

        let refreshed = oauth::refresh_access_token(
            &self.http,
            &self.token_url,
            &self.client_id,
            refresh_token,
        )
        .await?;

        persist::apply_refreshed_tokens(&mut auth_value, &refreshed)?;
        persist::atomic_write_json(path, &auth_value)?;

        load_codex_auth(path)
    }
}

#[must_use]
pub fn ping() -> &'static str {
    "pong"
}
