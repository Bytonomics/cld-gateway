#![forbid(unsafe_code)]

use serde::Serialize;
use serde_json::Value;
use std::fmt::Write as _;

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

#[must_use]
pub fn format_exchange_record(record: &ExchangeRecord) -> String {
    let mut output = String::new();
    writeln!(output, "=== HTTP exchange {} ===", record.request_id).expect("write to String");
    writeln!(output, "started_at_unix_ms: {}", record.started_at_unix_ms).expect("write to String");
    writeln!(output, "duration_ms: {}", record.duration_ms).expect("write to String");
    if let Some(meta) = &record.meta
        && let Some(resolution) = &meta.model_resolution
    {
        writeln!(output, "model_resolution: {resolution:?}").expect("write to String");
    }
    writeln!(output, "request.method: {}", record.request.method).expect("write to String");
    writeln!(output, "request.uri: {}", record.request.uri).expect("write to String");
    format_headers(&mut output, "request.headers", &record.request.headers);
    format_body(&mut output, "request.body", &record.request.body);
    writeln!(output, "response.status: {}", record.response.status).expect("write to String");
    format_headers(&mut output, "response.headers", &record.response.headers);
    format_body(&mut output, "response.body", &record.response.body);
    output.push('\n');
    output
}

fn format_headers(
    output: &mut String,
    prefix: &str,
    headers: &std::collections::BTreeMap<String, String>,
) {
    if headers.is_empty() {
        writeln!(output, "{prefix}:").expect("write to String");
        writeln!(output, "  (none)").expect("write to String");
        return;
    }
    writeln!(output, "{prefix}:").expect("write to String");
    for (name, value) in headers {
        writeln!(output, "  {name}: {value}").expect("write to String");
    }
}

fn format_body(output: &mut String, prefix: &str, body: &CapturedBody) {
    writeln!(
        output,
        "{prefix}.content_type: {}",
        body.content_type.as_deref().unwrap_or("(none)")
    )
    .expect("write to String");
    writeln!(output, "{prefix}.bytes_captured: {}", body.bytes_captured).expect("write to String");
    writeln!(output, "{prefix}.truncated: {}", body.truncated).expect("write to String");
    match &body.data {
        CapturedBodyData::Empty => {
            writeln!(output, "{prefix}.value: (empty)").expect("write to String");
        }
        CapturedBodyData::Json { value } => {
            writeln!(output, "{prefix}.json:").expect("write to String");
            let pretty = serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".into());
            for line in pretty.lines() {
                writeln!(output, "  {line}").expect("write to String");
            }
        }
        CapturedBodyData::Text { value } => {
            writeln!(output, "{prefix}.text:").expect("write to String");
            for line in value.lines() {
                writeln!(output, "  {line}").expect("write to String");
            }
        }
        CapturedBodyData::Binary { note } => {
            writeln!(output, "{prefix}.note: {note}").expect("write to String");
        }
    }
}
