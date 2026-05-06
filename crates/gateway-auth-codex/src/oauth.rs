#![forbid(unsafe_code)]

use crate::CodexAuthError;

#[derive(Debug, serde::Serialize)]
struct RefreshRequest<'a> {
    grant_type: &'a str,
    client_id: &'a str,
    refresh_token: &'a str,
}

#[derive(Debug, serde::Deserialize)]
#[allow(clippy::struct_field_names)]
pub struct RefreshResponse {
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub id_token: Option<String>,
}

/// Performs an OAuth 2.0 `refresh_token` grant against the Codex token endpoint.
///
/// # Errors
///
/// Returns an error if the HTTP request fails, the response status is not success, or the response
/// body does not contain a usable `access_token`.
pub async fn refresh_access_token(
    http: &reqwest::Client,
    token_url: &str,
    client_id: &str,
    refresh_token: &str,
) -> Result<RefreshResponse, CodexAuthError> {
    let payload = RefreshRequest {
        grant_type: "refresh_token",
        client_id,
        refresh_token,
    };

    let res = http
        .post(token_url)
        .form(&payload)
        .send()
        .await
        .map_err(|_| CodexAuthError::RefreshFailed)?;

    if !res.status().is_success() {
        return Err(CodexAuthError::RefreshFailed);
    }

    let parsed: RefreshResponse = res
        .json()
        .await
        .map_err(|_| CodexAuthError::RefreshUnexpectedResponse)?;

    if parsed.access_token.is_none() {
        return Err(CodexAuthError::RefreshUnexpectedResponse);
    }

    Ok(parsed)
}
