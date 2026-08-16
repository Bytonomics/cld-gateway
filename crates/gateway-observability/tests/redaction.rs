#![forbid(unsafe_code)]

use gateway_observability::exchange::{
    CapturedBody, CapturedBodyData, ExchangeRecord, HttpRequestRecord, HttpResponseRecord,
};
use gateway_observability::middleware::append_exchange_record;
use gateway_observability::middleware::append_human_readable_exchange_record;
use gateway_observability::redact::{redact_headers, redact_json_keys};
use http::HeaderMap;
use std::path::PathBuf;

#[test]
fn redact_headers_drops_sensitive_values() {
    let mut headers = HeaderMap::new();
    headers.insert(
        http::header::AUTHORIZATION,
        http::HeaderValue::from_static("Bearer super-secret-token"),
    );
    headers.insert(
        http::header::COOKIE,
        http::HeaderValue::from_static("session=super-secret-cookie"),
    );
    headers.insert(
        http::header::CONTENT_TYPE,
        http::HeaderValue::from_static("application/json"),
    );

    let redacted = redact_headers(&headers);
    assert_eq!(
        redacted.get("authorization").map(String::as_str),
        Some("[REDACTED]")
    );
    assert_eq!(
        redacted.get("cookie").map(String::as_str),
        Some("[REDACTED]")
    );
    assert_eq!(
        redacted.get("content-type").map(String::as_str),
        Some("application/json")
    );

    let serialized = serde_json::to_string(&redacted).expect("serialize headers");
    assert!(!serialized.contains("super-secret-token"));
    assert!(!serialized.contains("super-secret-cookie"));
}

#[test]
fn redact_json_keys_redacts_nested_token_fields() {
    let input = serde_json::json!({
        "access_token": "aaa",
        "nested": {
            "refresh_token": "bbb",
            "ok": 123,
            "arr": [{"id_token": "ccc"}],
        }
    });
    let out = redact_json_keys(input);
    let serialized = serde_json::to_string(&out).expect("serialize redacted json");
    assert!(!serialized.contains("\"aaa\""));
    assert!(!serialized.contains("\"bbb\""));
    assert!(!serialized.contains("\"ccc\""));
    assert!(serialized.contains("[REDACTED]"));
}

#[test]
fn append_exchange_record_writes_one_json_object_per_line() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path: PathBuf = dir.path().join("http-exchange.jsonl");

    let record = ExchangeRecord {
        request_id: "req-1".to_string(),
        started_at_unix_ms: 0,
        duration_ms: 1,
        meta: None,
        request: HttpRequestRecord {
            method: "GET".to_string(),
            uri: "/health".to_string(),
            headers: std::collections::BTreeMap::new(),
            body: CapturedBody {
                content_type: None,
                bytes_captured: 0,
                truncated: false,
                data: CapturedBodyData::Empty,
            },
        },
        response: HttpResponseRecord {
            status: 200,
            headers: std::collections::BTreeMap::new(),
            body: CapturedBody {
                content_type: Some("application/json".to_string()),
                bytes_captured: 2,
                truncated: false,
                data: CapturedBodyData::Text {
                    value: "{}".to_string(),
                },
            },
        },
    };

    append_exchange_record(&path, &record).expect("write first record");
    append_exchange_record(&path, &record).expect("write second record");

    let contents = std::fs::read_to_string(&path).expect("read jsonl");
    let lines: Vec<&str> = contents.lines().collect();
    assert_eq!(lines.len(), 2);
    for line in lines {
        let _: serde_json::Value = serde_json::from_str(line).expect("each line is json");
    }
}

#[test]
fn append_human_readable_exchange_record_writes_structured_multiline_text() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("http-exchange.log");
    let record = ExchangeRecord {
        request_id: "req-readable".to_string(),
        started_at_unix_ms: 10,
        duration_ms: 3,
        meta: None,
        request: HttpRequestRecord {
            method: "POST".to_string(),
            uri: "/v1/messages".to_string(),
            headers: std::collections::BTreeMap::new(),
            body: CapturedBody {
                content_type: Some("application/json".to_string()),
                bytes_captured: 15,
                truncated: false,
                data: CapturedBodyData::Json {
                    value: serde_json::json!({"messages": [{"role": "user", "content": "hello"}]}),
                },
            },
        },
        response: HttpResponseRecord {
            status: 200,
            headers: std::collections::BTreeMap::new(),
            body: CapturedBody {
                content_type: Some("application/json".to_string()),
                bytes_captured: 2,
                truncated: false,
                data: CapturedBodyData::Text {
                    value: "ok".to_string(),
                },
            },
        },
    };

    append_human_readable_exchange_record(&path, &record).expect("write readable log");
    let contents = std::fs::read_to_string(path).expect("read readable log");
    assert!(contents.contains("=== HTTP exchange req-readable ==="));
    assert!(contents.contains("request.method: POST"));
    assert!(contents.contains("request.uri: /v1/messages"));
    assert!(contents.contains("request.body.json:"));
    assert!(contents.contains("\"role\": \"user\""));
    assert!(contents.contains("response.status: 200"));
}
