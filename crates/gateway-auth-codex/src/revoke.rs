#![forbid(unsafe_code)]

use crate::auth_json::AuthJson;
use crate::auth_json::Tokens;
use crate::{CodexAuthError, load_auth_json};
use serde::Serialize;
use std::path::Path;
use std::time::Duration;

const REVOKE_TOKEN_URL: &str = "https://auth.openai.com/oauth/revoke";
const REVOKE_HTTP_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RevokeTokenKind {
    Access,
    Refresh,
}

impl RevokeTokenKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Access => "access_token",
            Self::Refresh => "refresh_token",
        }
    }

    fn client_id(self) -> Option<&'static str> {
        match self {
            Self::Access => None,
            Self::Refresh => Some("app_EMoamEEZ73f0CkXaXp7hrann"),
        }
    }
}

#[derive(Serialize)]
struct RevokeTokenRequest<'a> {
    token: &'a str,
    token_type_hint: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    client_id: Option<&'static str>,
}

pub fn logout(path: &Path) -> Result<bool, CodexAuthError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(CodexAuthError::IoWithPath {
            path: path.to_path_buf(),
            source: err,
        }),
    }
}

pub async fn logout_with_revoke(path: &Path) -> Result<bool, CodexAuthError> {
    let auth = match load_auth_json(path) {
        Ok(auth) => auth,
        Err(CodexAuthError::AuthNotFound { .. }) => return logout(path),
        Err(err) => return Err(err),
    };

    if let Some((token, kind)) = revocable_token(&auth)
        && let Err(err) = revoke_oauth_token(REVOKE_TOKEN_URL, token, kind).await
    {
        tracing::warn!("failed to revoke auth tokens during logout: {err}");
    }

    logout(path)
}

fn revocable_token(auth: &AuthJson) -> Option<(&str, RevokeTokenKind)> {
    let tokens: &Tokens = auth.tokens.as_ref()?;
    if let Some(refresh) = tokens
        .refresh_token
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        Some((refresh, RevokeTokenKind::Refresh))
    } else {
        tokens
            .access_token
            .as_deref()
            .filter(|value| !value.is_empty())
            .map(|token| (token, RevokeTokenKind::Access))
    }
}

async fn revoke_oauth_token(
    endpoint: &str,
    token: &str,
    kind: RevokeTokenKind,
) -> Result<(), std::io::Error> {
    let request = RevokeTokenRequest {
        token,
        token_type_hint: kind.as_str(),
        client_id: kind.client_id(),
    };

    let client = reqwest::Client::new();
    let response = client
        .post(endpoint)
        .header("Content-Type", "application/json")
        .timeout(REVOKE_HTTP_TIMEOUT)
        .json(&request)
        .send()
        .await
        .map_err(std::io::Error::other)?;

    let status = response.status();
    if status.is_success() {
        Ok(())
    } else {
        let body = response.text().await.unwrap_or_default();
        Err(std::io::Error::other(format!(
            "failed to revoke {}: {}: {}",
            kind.as_str(),
            status,
            body
        )))
    }
}
