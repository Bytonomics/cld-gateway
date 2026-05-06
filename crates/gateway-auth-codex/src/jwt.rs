#![forbid(unsafe_code)]

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use crate::CodexAuthError;

#[derive(Debug, serde::Deserialize)]
struct JwtClaims {
    exp: Option<i64>,
}

pub fn extract_exp_unverified(access_token: &str) -> Result<Option<i64>, CodexAuthError> {
    let mut parts = access_token.split('.');
    let _header = parts.next().ok_or(CodexAuthError::JwtMalformed)?;
    let payload_b64 = parts.next().ok_or(CodexAuthError::JwtMalformed)?;

    // We don't care about signature; we only read exp for observability/refresh scheduling.
    let payload_bytes = URL_SAFE_NO_PAD
        .decode(payload_b64.as_bytes())
        .map_err(|_| CodexAuthError::JwtDecode)?;

    let claims: JwtClaims =
        serde_json::from_slice(&payload_bytes).map_err(|_| CodexAuthError::JwtClaimsJson)?;

    Ok(claims.exp)
}
