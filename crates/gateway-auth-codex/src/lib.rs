// Crate: gateway-auth-codex
// Purpose: load/refresh Codex OAuth credentials from disk and provide safe auth snapshots.
// Allowed deps: gateway-core.
// Not allowed: axum/http routing, backend client code.

#![forbid(unsafe_code)]

mod auth_json;
mod jwt;
mod keyring_auth;
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

    #[error("failed to load Codex auth from keyring service {service} key {key}")]
    KeyringLoad {
        service: &'static str,
        key: String,
        #[source]
        source: keyring::Error,
    },

    #[error("failed to save Codex auth to keyring service {service} key {key}")]
    KeyringSave {
        service: &'static str,
        key: String,
        #[source]
        source: keyring::Error,
    },

    #[error("no Codex auth found in keyring ({service}/{key}) and no auth.json at {path}")]
    AuthNotFound {
        service: &'static str,
        key: String,
        path: PathBuf,
    },
}

#[derive(Debug, Clone)]
enum AuthStorage {
    Keyring {
        service: &'static str,
        key: String,
        fallback_path: PathBuf,
    },
    File {
        path: PathBuf,
    },
}

impl AuthStorage {
    fn persist_json_value(&self, value: &serde_json::Value) -> Result<(), CodexAuthError> {
        match self {
            AuthStorage::Keyring {
                service,
                key,
                fallback_path,
            } => {
                let serialized = serde_json::to_string(value).map_err(CodexAuthError::Json)?;
                keyring_auth::save_codex_auth_json_to_keyring_key(key, &serialized).map_err(
                    |err| match err {
                        keyring_auth::KeyringAuthError::NotFound => CodexAuthError::KeyringSave {
                            service,
                            key: key.clone(),
                            source: keyring::Error::NoEntry,
                        },
                        keyring_auth::KeyringAuthError::Keyring(source) => {
                            CodexAuthError::KeyringSave {
                                service,
                                key: key.clone(),
                                source,
                            }
                        }
                    },
                )?;

                // If we successfully wrote to keyring, match Codex behavior: best-effort cleanup
                // of the file fallback (if any). Failures here should not break refresh.
                let _ = std::fs::remove_file(fallback_path);
                Ok(())
            }
            AuthStorage::File { path } => {
                persist::atomic_write_json(path, value)?;
                Ok(())
            }
        }
    }
}

fn load_auth_json_auto() -> Result<(serde_json::Value, AuthStorage), CodexAuthError> {
    let (codex_home, key) = keyring_auth::compute_default_key();
    let service = keyring_auth::keyring_service();

    match keyring_auth::load_codex_auth_json_from_keyring() {
        Ok(json) => {
            let value: serde_json::Value = serde_json::from_str(&json)?;
            Ok((
                value,
                AuthStorage::Keyring {
                    service,
                    key,
                    fallback_path: codex_home.join("auth.json"),
                },
            ))
        }
        Err(keyring_auth::KeyringAuthError::NotFound) => {
            let path = paths::default_auth_json_path();
            let bytes = match std::fs::read(&path) {
                Ok(bytes) => bytes,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    return Err(CodexAuthError::AuthNotFound { service, key, path });
                }
                Err(err) => {
                    return Err(CodexAuthError::IoWithPath { path, source: err });
                }
            };
            let value: serde_json::Value = serde_json::from_slice(&bytes)?;
            Ok((value, AuthStorage::File { path }))
        }
        Err(keyring_auth::KeyringAuthError::Keyring(err)) => Err(CodexAuthError::KeyringLoad {
            service,
            key,
            source: err,
        }),
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
    let (value, _) = load_auth_json_auto()?;
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
    let (value, _) = load_auth_json_auto()?;
    let auth: auth_json::AuthJson = serde_json::from_value(value)?;

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

fn load_codex_auth_from_parsed(
    auth: auth_json::AuthJson,
) -> Result<CodexAuthSnapshot, CodexAuthError> {
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
        let (mut auth_value, storage) = load_auth_json_auto()?;
        self.refresh_and_persist_value(&mut auth_value, &storage)
            .await
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

    async fn refresh_and_persist_value(
        &self,
        auth_value: &mut serde_json::Value,
        storage: &AuthStorage,
    ) -> Result<CodexAuthSnapshot, CodexAuthError> {
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

        persist::apply_refreshed_tokens(auth_value, &refreshed)?;
        storage.persist_json_value(auth_value)?;

        // Re-parse from the updated in-memory auth so callers get a consistent view even if
        // persistence destination differs from the legacy file path.
        let auth: auth_json::AuthJson = serde_json::from_value(auth_value.clone())?;
        load_codex_auth_from_parsed(auth)
    }
}

#[must_use]
pub fn ping() -> &'static str {
    "pong"
}
