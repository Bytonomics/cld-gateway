#![forbid(unsafe_code)]

use crate::exchange::{
    CapturedBody, CapturedBodyData, ExchangeMeta, ExchangeRecord, HttpRequestRecord,
    HttpResponseRecord,
};
use crate::paths::default_exchange_log_path;
use crate::redact::{redact_headers, redact_json_keys};
use axum::body::Body;
use axum::middleware::Next;
use bytes::Bytes;
use futures_util::StreamExt as _;
use gateway_core::RequestId;
use http::{Request, Response};
use serde_json::Value;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use tokio::sync::oneshot;
use tracing::info;
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
    let (request_body_done, request_body_for_downstream) = capture_body_streaming(
        request_parts.headers.get(http::header::CONTENT_TYPE),
        request_body,
    );

    req = Request::from_parts(request_parts, request_body_for_downstream);
    req.extensions_mut().insert(request_id.clone());

    let mut res = next.run(req).await;
    res.headers_mut().insert(
        "x-proxy-request-id",
        request_id
            .0
            .parse()
            .unwrap_or_else(|_| http::HeaderValue::from_static("invalid-request-id")),
    );

    let (response_parts, response_body) = res.into_parts();
    let model_resolution = response_parts
        .extensions
        .get::<gateway_core::config::ModelResolution>()
        .cloned();
    let (response_body_done, response_body_for_client) = capture_body_streaming(
        response_parts.headers.get(http::header::CONTENT_TYPE),
        response_body,
    );

    let res = Response::from_parts(response_parts, response_body_for_client);
    let response_status = res.status().as_u16();
    let response_headers_redacted = redact_headers(res.headers());

    // Log after both request and response bodies are fully consumed (or dropped), so we can
    // capture bodies up to the limit *without* breaking downstream processing for oversized bodies.
    let log_path = config.log_path.clone();
    tokio::spawn(async move {
        let request_body_capture = request_body_done.await.unwrap_or(CapturedBody {
            content_type: None,
            bytes_captured: 0,
            truncated: false,
            data: CapturedBodyData::Empty,
        });

        let response_body_capture = response_body_done.await.unwrap_or(CapturedBody {
            content_type: None,
            bytes_captured: 0,
            truncated: false,
            data: CapturedBodyData::Empty,
        });

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
            meta: if model_resolution.is_some() {
                Some(ExchangeMeta { model_resolution })
            } else {
                None
            },
            request: HttpRequestRecord {
                method: request_method,
                uri: request_uri,
                headers: request_headers_redacted,
                body: request_body_capture,
            },
            response: HttpResponseRecord {
                status: response_status,
                headers: response_headers_redacted,
                body: response_body_capture,
            },
        };

        let _ = append_exchange_record(&log_path, &exchange);
    });

    res
}

fn capture_body_streaming(
    content_type: Option<&http::HeaderValue>,
    body: Body,
) -> (oneshot::Receiver<CapturedBody>, Body) {
    let ct = content_type
        .and_then(|v| v.to_str().ok())
        .map(std::string::ToString::to_string);

    let (tx, rx) = oneshot::channel::<CapturedBody>();
    let captured: Vec<u8> = Vec::new();
    let truncated = false;

    let stream = body.into_data_stream();
    let tee_stream = futures_util::stream::unfold(
        (stream, tx, ct.clone(), captured, truncated),
        |(mut stream, tx, ct, mut captured, mut truncated)| async move {
            match stream.next().await {
                Some(Ok(chunk)) => {
                    if !truncated {
                        let was_truncated = truncated;
                        let remaining = BODY_LIMIT_BYTES.saturating_sub(captured.len());
                        if remaining == 0 {
                            truncated = true;
                        } else if chunk.len() <= remaining {
                            captured.extend_from_slice(&chunk);
                        } else {
                            captured.extend_from_slice(&chunk[..remaining]);
                            truncated = true;
                        }
                        if !was_truncated && truncated {
                            info!("body capture skipped: exceeded {BODY_LIMIT_BYTES} bytes limit");
                        }
                    }
                    Some((
                        Ok::<Bytes, axum::Error>(chunk),
                        (stream, tx, ct, captured, truncated),
                    ))
                }
                Some(Err(err)) => Some((
                    Err::<Bytes, axum::Error>(err),
                    (stream, tx, ct, captured, truncated),
                )),
                None => {
                    let captured_body = build_captured_body(ct, &captured, truncated);
                    let _ = tx.send(captured_body);
                    None
                }
            }
        },
    );

    (rx, Body::from_stream(tee_stream))
}

fn build_captured_body(ct: Option<String>, captured: &[u8], truncated: bool) -> CapturedBody {
    let bytes_captured = captured.len();

    let data = if truncated {
        CapturedBodyData::Binary {
            note: format!("body capture skipped: exceeded {BODY_LIMIT_BYTES} bytes limit"),
        }
    } else if captured.is_empty() {
        CapturedBodyData::Empty
    } else if let Ok(as_str) = std::str::from_utf8(captured) {
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

    CapturedBody {
        content_type: ct,
        bytes_captured,
        truncated,
        data,
    }
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
    use super::{BODY_LIMIT_BYTES, build_captured_body};
    use crate::exchange::CapturedBodyData;

    #[tokio::test]
    async fn build_captured_body_marks_oversize_capture_as_skipped() {
        let captured = build_captured_body(None, b"", true);
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
