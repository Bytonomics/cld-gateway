#![forbid(unsafe_code)]

use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct AuthJson {
    pub tokens: Tokens,
}

#[derive(Debug, Deserialize)]
pub struct Tokens {
    #[serde(default)]
    pub access_token: Option<String>,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub id_token: Option<String>,
    #[serde(default)]
    pub account_id: Option<String>,
}
