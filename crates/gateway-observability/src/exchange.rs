#![forbid(unsafe_code)]

use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Serialize, Clone, Default)]
pub struct ExchangeMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_resolution: Option<gateway_core::config::ModelResolution>,
}

#[derive(Debug, Serialize, Clone)]
pub struct ExchangeRecord {
    pub request_id: String,
    pub started_at_unix_ms: u128,
    pub duration_ms: u128,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<ExchangeMeta>,
    pub request: HttpRequestRecord,
    pub response: HttpResponseRecord,
}

#[derive(Debug, Serialize, Clone)]
pub struct HttpRequestRecord {
    pub method: String,
    pub uri: String,
    pub headers: std::collections::BTreeMap<String, String>,
    pub body: CapturedBody,
}

#[derive(Debug, Serialize, Clone)]
pub struct HttpResponseRecord {
    pub status: u16,
    pub headers: std::collections::BTreeMap<String, String>,
    pub body: CapturedBody,
}

#[derive(Debug, Serialize, Clone)]
pub struct CapturedBody {
    pub content_type: Option<String>,
    pub bytes_captured: usize,
    pub truncated: bool,
    #[serde(flatten)]
    pub data: CapturedBodyData,
}

#[derive(Debug, Serialize, Clone)]
#[serde(tag = "body_type", rename_all = "snake_case")]
pub enum CapturedBodyData {
    Empty,
    Json { value: Value },
    Text { value: String },
    Binary { note: String },
}
