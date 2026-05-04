#![forbid(unsafe_code)]

use http::HeaderMap;
use serde_json::Value;
use std::collections::BTreeMap;

const REDACTED_VALUE: &str = "[REDACTED]";

#[must_use]
pub fn redact_headers(headers: &HeaderMap) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (name, value) in headers {
        let name_str = name.as_str().to_ascii_lowercase();
        let redacted = matches!(
            name_str.as_str(),
            "authorization" | "cookie" | "set-cookie" | "proxy-authorization"
        );

        if redacted {
            out.insert(name_str, REDACTED_VALUE.to_string());
            continue;
        }

        let value_str = value.to_str().unwrap_or("[NON-UTF8]");
        out.insert(name_str, value_str.to_string());
    }
    out
}

#[must_use]
pub fn redact_json_keys(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                let key_lower = k.to_ascii_lowercase();
                let should_redact = matches!(
                    key_lower.as_str(),
                    "access_token" | "refresh_token" | "id_token" | "token"
                );
                if should_redact {
                    out.insert(k, Value::String(REDACTED_VALUE.to_string()));
                } else {
                    out.insert(k, redact_json_keys(v));
                }
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.into_iter().map(redact_json_keys).collect()),
        other => other,
    }
}
