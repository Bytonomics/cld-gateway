#![forbid(unsafe_code)]

use gateway_core::Secret;

#[derive(Clone)]
pub struct CodexBackendRequest {
    pub access_token: Secret<String>,
    pub account_id: String,
    pub model: String,
    pub input_text: String,
}

#[derive(Debug, Clone)]
pub struct CodexBackendResponse {
    pub status: u16,
    pub body: String,
}
