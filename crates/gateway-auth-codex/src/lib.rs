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
mod revoke;

use gateway_core::Secret;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use url::Url;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexAuthSnapshot {
    pub account_id: String,
    pub has_access_token: bool,
    pub has_refresh_token: bool,
    pub expires_at_unix_seconds: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatewayLoginMethod {
    Chatgpt,
    ApiKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayAuthStatus {
    pub login_method: GatewayLoginMethod,
    pub account_id: Option<String>,
    pub has_access_token: bool,
    pub has_refresh_token: bool,
    pub has_openai_api_key: bool,
}

impl GatewayAuthStatus {
    #[must_use]
    pub fn ready_for_messages(&self) -> bool {
        matches!(self.login_method, GatewayLoginMethod::Chatgpt)
            && self.has_access_token
            && self.has_refresh_token
            && self.account_id.is_some()
    }

    #[must_use]
    pub fn ready_for_models(&self) -> bool {
        self.has_openai_api_key || self.ready_for_messages()
    }
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

    #[error("token refresh transport failed: {message}")]
    RefreshTransportFailed { message: String },

    #[error("token refresh failed with status {status}: {body}")]
    RefreshFailed { status: u16, body: String },

    #[error("token refresh rejected with code {code:?}: {body}")]
    RefreshUnauthorized { code: Option<String>, body: String },

    #[error("token endpoint returned unexpected response: {body}")]
    RefreshUnexpectedResponse { body: String },

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

impl CodexAuthError {
    #[must_use]
    pub fn is_permanent_refresh_failure(&self) -> bool {
        matches!(self, Self::RefreshUnauthorized { .. })
    }
}

pub(crate) fn load_auth_json(path: &Path) -> Result<auth_json::AuthJson, CodexAuthError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(CodexAuthError::AuthNotFound {
                path: path.to_path_buf(),
            });
        }
        Err(err) => {
            return Err(CodexAuthError::IoWithPath {
                path: path.to_path_buf(),
                source: err,
            });
        }
    };
    Ok(serde_json::from_slice::<auth_json::AuthJson>(&bytes)?)
}

pub(crate) fn load_auth_json_default_path() -> Result<auth_json::AuthJson, CodexAuthError> {
    load_auth_json(&paths::default_auth_json_path())
}

fn auth_status_from_parsed(auth: &auth_json::AuthJson) -> GatewayAuthStatus {
    let has_openai_api_key = auth.openai_api_key.is_some();
    let login_method = if has_openai_api_key {
        GatewayLoginMethod::ApiKey
    } else {
        GatewayLoginMethod::Chatgpt
    };
    let tokens = auth.tokens.as_ref();
    let has_access_token = tokens
        .and_then(|tokens| tokens.access_token.as_ref())
        .is_some();
    let has_refresh_token = tokens
        .and_then(|tokens| tokens.refresh_token.as_ref())
        .is_some();
    let account_id = tokens
        .and_then(|tokens| tokens.account_id.clone())
        .or_else(|| {
            tokens
                .and_then(|tokens| tokens.id_token.as_deref())
                .and_then(jwt::extract_chatgpt_account_id_unverified)
        });

    GatewayAuthStatus {
        login_method,
        account_id,
        has_access_token,
        has_refresh_token,
        has_openai_api_key,
    }
}

/// Loads gateway auth status from the default auth path.
///
/// # Errors
///
/// Returns `AuthNotFound` if auth is missing, or parse/io errors otherwise.
pub fn load_gateway_auth_status_default_path() -> Result<Option<GatewayAuthStatus>, CodexAuthError>
{
    match load_auth_json_default_path() {
        Ok(auth) => Ok(Some(auth_status_from_parsed(&auth))),
        Err(CodexAuthError::AuthNotFound { .. }) => Ok(None),
        Err(err) => Err(err),
    }
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
    let auth = load_auth_json_default_path()?;
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
    Ok(load_auth_json_default_path()?
        .openai_api_key
        .map(Secret::new))
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

/// Removes the default auth file without revoking tokens.
///
/// # Errors
///
/// Returns an error if removing the file fails for reasons other than it not existing.
pub fn logout_default_path() -> Result<bool, CodexAuthError> {
    revoke::logout(&paths::default_auth_json_path())
}

/// Best-effort revocation of the current default auth file, then removal of the file.
///
/// # Errors
///
/// Returns an error if the file removal fails for reasons other than not existing.
pub async fn logout_with_revoke_default_path() -> Result<bool, CodexAuthError> {
    revoke::logout_with_revoke(&paths::default_auth_json_path()).await
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
    load_credentials(&paths::default_auth_json_path())
}

/// Loads access token + account id from `path` in one IO/parse pass.
///
/// # Errors
///
/// Returns an error if loading/parsing fails or required fields are missing.
pub fn load_credentials(path: &Path) -> Result<CodexCredentials, CodexAuthError> {
    let bytes = std::fs::read(path).map_err(|e| CodexAuthError::IoWithPath {
        path: path.to_path_buf(),
        source: e,
    })?;
    let auth: auth_json::AuthJson = serde_json::from_slice(&bytes)?;
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
    state: Arc<CodexAuthManagerState>,
}

struct CodexAuthManagerState {
    refresh_lock: tokio::sync::Mutex<()>,
    permanent_refresh_failure: tokio::sync::Mutex<Option<PermanentRefreshFailure>>,
}

#[derive(Debug, Clone)]
struct PermanentRefreshFailure {
    auth_fingerprint: String,
    code: Option<String>,
    body: String,
}

impl PermanentRefreshFailure {
    fn error(&self) -> CodexAuthError {
        CodexAuthError::RefreshUnauthorized {
            code: self.code.clone(),
            body: self.body.clone(),
        }
    }
}

impl Default for CodexAuthManager {
    fn default() -> Self {
        Self {
            // Per plan Day 8.
            token_url: "https://auth.openai.com/oauth/token".to_string(),
            client_id: "app_EMoamEEZ73f0CkXaXp7hrann".to_string(),
            http: reqwest::Client::new(),
            state: Arc::new(CodexAuthManagerState {
                refresh_lock: tokio::sync::Mutex::new(()),
                permanent_refresh_failure: tokio::sync::Mutex::new(None),
            }),
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
        let _guard = self.state.refresh_lock.lock().await;
        let auth_before = load_auth_json(path)?;
        let auth_before_fingerprint = serde_json::to_string(&auth_before)?;

        if let Some(cached) = self
            .cached_permanent_failure(&auth_before_fingerprint)
            .await
        {
            return Err(cached.error());
        }

        let auth_after = load_auth_json(path)?;
        let auth_after_fingerprint = serde_json::to_string(&auth_after)?;
        if auth_after_fingerprint != auth_before_fingerprint {
            self.clear_cached_permanent_failure_if_changed(&auth_after_fingerprint)
                .await;
            return load_codex_auth(path);
        }

        let mut auth_value: serde_json::Value = serde_json::to_value(&auth_after)?;

        let refresh_token = auth_value
            .pointer("/tokens/refresh_token")
            .and_then(|v| v.as_str())
            .ok_or(CodexAuthError::MissingField("tokens.refresh_token"))?;

        let refreshed = match oauth::refresh_access_token(
            &self.http,
            &self.token_url,
            &self.client_id,
            refresh_token,
        )
        .await
        {
            Ok(refreshed) => refreshed,
            Err(err) => {
                if let CodexAuthError::RefreshUnauthorized { code, body } = &err {
                    let mut guard = self.state.permanent_refresh_failure.lock().await;
                    *guard = Some(PermanentRefreshFailure {
                        auth_fingerprint: auth_after_fingerprint,
                        code: code.clone(),
                        body: body.clone(),
                    });
                }
                return Err(err);
            }
        };

        persist::apply_refreshed_tokens(&mut auth_value, &refreshed)?;
        persist::atomic_write_json(path, &auth_value)?;
        self.clear_cached_permanent_failure_if_changed(&serde_json::to_string(&auth_after)?)
            .await;

        load_codex_auth(path)
    }

    // (helper removed) keyring persistence is deferred.
}

impl CodexAuthManager {
    async fn cached_permanent_failure(
        &self,
        auth_fingerprint: &str,
    ) -> Option<PermanentRefreshFailure> {
        let guard = self.state.permanent_refresh_failure.lock().await;
        guard
            .as_ref()
            .filter(|failure| failure.auth_fingerprint == auth_fingerprint)
            .cloned()
    }

    async fn clear_cached_permanent_failure_if_changed(&self, auth_fingerprint: &str) {
        let mut guard = self.state.permanent_refresh_failure.lock().await;
        if guard
            .as_ref()
            .is_some_and(|failure| failure.auth_fingerprint != auth_fingerprint)
        {
            *guard = None;
        }
    }
}

#[must_use]
pub fn ping() -> &'static str {
    "pong"
}
