#![forbid(unsafe_code)]

use crate::CodexAuthError;
use gateway_net::GatewayHttpClient;

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
    http: &GatewayHttpClient,
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
        .map_err(|source| CodexAuthError::RefreshTransportFailed {
            message: source.to_string(),
        })?
        .form(&payload)
        .execute()
        .await
        .map_err(|source| CodexAuthError::RefreshTransportFailed {
            message: source.to_string(),
        })?;

    let status = res.status();
    let body = res
        .text()
        .await
        .map_err(|source| CodexAuthError::RefreshUnexpectedResponse {
            body: source.to_string(),
        })?;

    if !status.is_success() {
        if status.as_u16() == 401 {
            return Err(CodexAuthError::RefreshUnauthorized {
                code: refresh_error_code(&body),
                body,
            });
        }
        return Err(CodexAuthError::RefreshFailed {
            status: status.as_u16(),
            body,
        });
    }

    let parsed: RefreshResponse = serde_json::from_str(&body)
        .map_err(|_| CodexAuthError::RefreshUnexpectedResponse { body: body.clone() })?;

    if parsed.access_token.is_none() {
        return Err(CodexAuthError::RefreshUnexpectedResponse { body });
    }

    Ok(parsed)
}

fn refresh_error_code(body: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(body).ok()?;
    let error = value.get("error")?;
    match error {
        serde_json::Value::Object(map) => map
            .get("code")
            .and_then(serde_json::Value::as_str)
            .map(ToString::to_string),
        serde_json::Value::String(code) => Some(code.clone()),
        _ => value
            .get("code")
            .and_then(serde_json::Value::as_str)
            .map(ToString::to_string),
    }
}
