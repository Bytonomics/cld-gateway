#![forbid(unsafe_code)]

use crate::exchange::{
    CapturedBody, CapturedBodyData, ExchangeRecord, HttpRequestRecord, HttpResponseRecord,
};
use crate::paths::default_exchange_log_path;
use crate::redact::{redact_headers, redact_json_keys};
use axum::body::{Body, to_bytes};
use axum::middleware::Next;
use gateway_core::RequestId;
use http::{Request, Response};
use serde_json::Value;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use uuid::Uuid;

const BODY_LIMIT_BYTES: usize = 5 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct CaptureConfig {
    pub log_path: PathBuf,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            log_path: default_exchange_log_path(),
        }
    }
}

pub async fn capture_http_exchange(
    mut req: Request<Body>,
    next: Next,
    config: CaptureConfig,
) -> Response<Body> {
    let started_at = SystemTime::now();
    let request_id = RequestId(Uuid::new_v4().to_string());

    let (request_parts, request_body) = req.into_parts();
    let request_method = request_parts.method.to_string();
    let request_uri = request_parts.uri.to_string();
    let request_headers_redacted = redact_headers(&request_parts.headers);
    let (request_body_capture, request_body_for_downstream) = capture_body(
        request_parts.headers.get(http::header::CONTENT_TYPE),
        request_body,
    )
    .await;

    req = Request::from_parts(request_parts, request_body_for_downstream);

    let mut res = next.run(req).await;
    res.headers_mut().insert(
        "x-proxy-request-id",
        request_id
            .0
            .parse()
            .unwrap_or_else(|_| http::HeaderValue::from_static("invalid-request-id")),
    );

    let (response_parts, response_body) = res.into_parts();
    let (response_body_capture, response_body_for_client) = capture_body(
        response_parts.headers.get(http::header::CONTENT_TYPE),
        response_body,
    )
    .await;

    let res = Response::from_parts(response_parts, response_body_for_client);

    let duration_ms = started_at
        .elapsed()
        .unwrap_or(Duration::from_millis(0))
        .as_millis();
    let started_at_unix_ms = started_at
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::from_millis(0))
        .as_millis();

    let exchange = ExchangeRecord {
        request_id: request_id.0.clone(),
        started_at_unix_ms,
        duration_ms,
        request: HttpRequestRecord {
            method: request_method,
            uri: request_uri,
            headers: request_headers_redacted,
            body: request_body_capture,
        },
        response: HttpResponseRecord {
            status: res.status().as_u16(),
            headers: redact_headers(res.headers()),
            body: response_body_capture,
        },
    };

    let _ = append_exchange_record(&config.log_path, &exchange);

    res
}

async fn capture_body(
    content_type: Option<&http::HeaderValue>,
    body: Body,
) -> (CapturedBody, Body) {
    let ct = content_type
        .and_then(|v| v.to_str().ok())
        .map(std::string::ToString::to_string);

    // `to_bytes` enforces a limit; if exceeded it returns an error. We treat that as truncation.
    let bytes_result = to_bytes(body, BODY_LIMIT_BYTES).await;
    let (bytes, truncated, capture_skipped_due_to_size) = match bytes_result {
        Ok(b) => (b, false, false),
        Err(_) => (bytes::Bytes::new(), true, true),
    };

    let bytes_captured = bytes.len();

    let data = if capture_skipped_due_to_size {
        CapturedBodyData::Binary {
            note: format!("body capture skipped: exceeded {BODY_LIMIT_BYTES} bytes limit"),
        }
    } else if bytes.is_empty() {
        CapturedBodyData::Empty
    } else if let Ok(as_str) = std::str::from_utf8(&bytes) {
        // Try JSON first (regardless of header; Claude Code payloads may omit content-type).
        match serde_json::from_str::<Value>(as_str) {
            Ok(json) => CapturedBodyData::Json {
                value: redact_json_keys(json),
            },
            Err(_) => CapturedBodyData::Text {
                value: as_str.to_string(),
            },
        }
    } else {
        CapturedBodyData::Binary {
            note: "non-utf8 body captured as metadata only".to_string(),
        }
    };

    (
        CapturedBody {
            content_type: ct,
            bytes_captured,
            truncated,
            data,
        },
        Body::from(bytes),
    )
}

/// Appends a single exchange record as one JSON object per line.
///
/// # Errors
///
/// Returns an error if the parent directory cannot be created, the file cannot be opened,
/// or the record cannot be written to disk.
pub fn append_exchange_record(path: &Path, record: &ExchangeRecord) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let line = serde_json::to_string(record).unwrap_or_else(|_| "{}".to_string());
    f.write_all(line.as_bytes())?;
    f.write_all(b"\n")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{BODY_LIMIT_BYTES, capture_body};
    use crate::exchange::CapturedBodyData;
    use axum::body::Body;

    #[tokio::test]
    async fn capture_body_marks_oversize_capture_as_skipped() {
        let payload = "x".repeat(BODY_LIMIT_BYTES + 1);
        let (captured, _forwarded) = capture_body(None, Body::from(payload)).await;

        assert!(captured.truncated);
        match captured.data {
            CapturedBodyData::Binary { note } => {
                assert!(note.contains("body capture skipped"));
                assert!(note.contains(&BODY_LIMIT_BYTES.to_string()));
            }
            other => panic!("expected Binary note for oversize body, got: {other:?}"),
        }
    }
}
