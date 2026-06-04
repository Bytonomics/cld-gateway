#![forbid(unsafe_code)]

#[must_use]
pub fn parse_backend_failure_event(event_name: &str, data: &str) -> Option<String> {
    if event_name != "error" && event_name != "response.failed" {
        return None;
    }

    let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
        return non_empty(data).map(|message| format!("{event_name}: {message}"));
    };

    let message = find_string_field(&value, "message")
        .or_else(|| find_string_field(&value, "error"))
        .or_else(|| find_string_field(&value, "code"))
        .unwrap_or_else(|| compact_json(&value));

    Some(format!("{event_name}: {message}"))
}

fn non_empty(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then_some(trimmed)
}

fn find_string_field(value: &serde_json::Value, field: &str) -> Option<String> {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(text) = map.get(field).and_then(|value| value.as_str()) {
                return Some(text.to_string());
            }
            map.values()
                .find_map(|child| find_string_field(child, field))
        }
        serde_json::Value::Array(items) => items
            .iter()
            .find_map(|child| find_string_field(child, field)),
        serde_json::Value::String(_)
        | serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_) => None,
    }
}

fn compact_json(value: &serde_json::Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "unparseable backend error".to_string())
}

#[cfg(test)]
mod tests {
    use super::parse_backend_failure_event;

    #[test]
    fn ignores_non_failure_events() {
        assert_eq!(parse_backend_failure_event("response.created", "{}"), None);
    }

    #[test]
    fn parses_top_level_error_message() {
        let message =
            parse_backend_failure_event("error", r#"{"type":"error","message":"bad request"}"#)
                .expect("failure");
        assert_eq!(message, "error: bad request");
    }

    #[test]
    fn parses_nested_response_failed_message() {
        let message = parse_backend_failure_event(
            "response.failed",
            r#"{"type":"response.failed","response":{"error":{"code":"invalid","message":"model unavailable"}}}"#,
        )
        .expect("failure");
        assert_eq!(message, "response.failed: model unavailable");
    }
}
