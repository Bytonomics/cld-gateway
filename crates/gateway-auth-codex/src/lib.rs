// Crate: gateway-auth-codex
// Purpose: load/refresh Codex OAuth credentials from disk and provide safe auth snapshots.
// Allowed deps: gateway-core.
// Not allowed: axum/http routing, backend client code.

#![forbid(unsafe_code)]

mod auth_json;
mod jwt;
pub mod login;
pub mod oauth;
pub mod paths;
mod persist;

use gateway_core::Secret;
use std::path::Path;
use std::path::PathBuf;
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
    #[error("io error")]
    Io(#[from] std::io::Error),

    #[error("failed to read auth.json at {path}")]
    IoWithPath {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

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

    #[error("no gateway auth found at {path}")]
    AuthNotFound { path: PathBuf },

    #[error("login timed out waiting for browser callback")]
    LoginTimeout,

    #[error("login callback missing code/state")]
    LoginInvalidCallback,

    #[error("login callback state mismatch")]
    LoginStateMismatch,

    #[error("token exchange failed with status {0}")]
    LoginTokenExchangeFailed(u16),
}

fn load_auth_json_default_path() -> Result<serde_json::Value, CodexAuthError> {
    let path = paths::default_auth_json_path();
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(CodexAuthError::AuthNotFound { path });
        }
        Err(err) => {
            return Err(CodexAuthError::IoWithPath { path, source: err });
        }
    };
    Ok(serde_json::from_slice::<serde_json::Value>(&bytes)?)
}

/// Loads Codex auth from the provided `auth.json` path and returns a safe snapshot.
///
/// # Errors
///
/// Returns an error if the file cannot be read, parsed, or does not contain the required fields.
pub fn load_codex_auth(path: &Path) -> Result<CodexAuthSnapshot, CodexAuthError> {
    let bytes = std::fs::read(path).map_err(|e| CodexAuthError::IoWithPath {
        path: path.to_path_buf(),
        source: e,
    })?;
    let auth: auth_json::AuthJson = serde_json::from_slice(&bytes)?;

    load_codex_auth_from_parsed(auth)
}

fn load_codex_auth_from_parsed(
    auth: auth_json::AuthJson,
) -> Result<CodexAuthSnapshot, CodexAuthError> {
    let tokens = auth.tokens.ok_or(CodexAuthError::MissingField("tokens"))?;

    // `id_token` is part of Codex auth.json; we intentionally don't expose it in the snapshot, but
    // we do read it so it remains covered by parsing/fixtures and doesn't rot.
    let _id_token_present = tokens.id_token.is_some();

    let access_token = tokens
        .access_token
        .as_deref()
        .ok_or(CodexAuthError::MissingField("tokens.access_token"))?;
    let refresh_token_present = tokens.refresh_token.is_some();

    let account_id = tokens
        .account_id
        .or_else(|| {
            tokens
                .id_token
                .as_deref()
                .and_then(jwt::extract_chatgpt_account_id_unverified)
        })
        .ok_or(CodexAuthError::MissingField("tokens.account_id"))?;

    let exp = jwt::extract_exp_unverified(access_token)?;

    Ok(CodexAuthSnapshot {
        account_id,
        has_access_token: true,
        has_refresh_token: refresh_token_present,
        expires_at_unix_seconds: exp,
    })
}

/// Loads gateway auth from the default path (`$GATEWAY_HOME/auth.json` or `~/.gateway/auth.json`).
///
/// # Errors
///
/// Returns an error if loading/parsing fails or required fields are missing.
pub fn load_codex_auth_default_path() -> Result<CodexAuthSnapshot, CodexAuthError> {
    let value = load_auth_json_default_path()?;
    let auth: auth_json::AuthJson = serde_json::from_value(value)?;
    load_codex_auth_from_parsed(auth)
}

/// Loads the current access token from the default auth path.
///
/// This is intentionally a minimal helper for early backend work; Day 8 introduces refresh/persistence.
///
/// # Errors
///
/// Returns an error if loading/parsing fails or required fields are missing.
pub fn load_access_token_default_path() -> Result<Secret<String>, CodexAuthError> {
    Ok(load_credentials_default_path()?.access_token)
}

/// Loads `OPENAI_API_KEY` from the gateway auth.json if present.
///
/// # Errors
///
/// Returns an error if auth.json cannot be read or parsed.
pub fn load_openai_api_key_default_path() -> Result<Option<Secret<String>>, CodexAuthError> {
    let value = load_auth_json_default_path()?;
    let auth: auth_json::AuthJson = serde_json::from_value(value)?;
    Ok(auth.openai_api_key.map(Secret::new))
}

/// Persists `OPENAI_API_KEY` into the gateway auth.json.
///
/// This mirrors Codex’s file format key name (`OPENAI_API_KEY`), but stores it under `~/.gateway/`.
///
/// # Errors
///
/// Returns an error if the auth.json cannot be written.
pub fn write_openai_api_key_default_path(api_key: &str) -> Result<(), CodexAuthError> {
    let auth = auth_json::AuthJson {
        auth_mode: Some("api_key".to_string()),
        openai_api_key: Some(api_key.to_string()),
        tokens: None,
        last_refresh: None,
    };
    let value = serde_json::to_value(auth)?;
    let path = paths::default_auth_json_path();
    persist::atomic_write_json(&path, &value)?;
    Ok(())
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
    let value = load_auth_json_default_path()?;
    let auth: auth_json::AuthJson = serde_json::from_value(value)?;
    let tokens = auth.tokens.ok_or(CodexAuthError::MissingField("tokens"))?;

    let access_token = tokens
        .access_token
        .ok_or(CodexAuthError::MissingField("tokens.access_token"))?;

    let account_id = tokens
        .account_id
        .or_else(|| {
            tokens
                .id_token
                .as_deref()
                .and_then(jwt::extract_chatgpt_account_id_unverified)
        })
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

    // (helper removed) keyring persistence is deferred.
}

#[must_use]
pub fn ping() -> &'static str {
    "pong"
}
