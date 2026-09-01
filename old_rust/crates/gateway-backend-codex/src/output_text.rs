#![forbid(unsafe_code)]

#[must_use]
pub fn extract_text_from_data(data: &str) -> Option<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
        return Some(data.to_string());
    };

    let mut last = None;
    extract_last_text_from_value(&value, &mut last);
    last
}

#[must_use]
pub fn parse_output_item_message_texts(event_name: &str, data: &str) -> Vec<String> {
    if event_name != "response.output_item.done" && event_name != "response.output_item.added" {
        return Vec::new();
    }

    let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
        return Vec::new();
    };
    let Some(item) = value.get("item").or_else(|| {
        value
            .get("response")
            .and_then(|response| response.get("item"))
    }) else {
        return Vec::new();
    };

    message_item_output_texts(item)
}

#[must_use]
pub fn message_item_output_texts(item: &serde_json::Value) -> Vec<String> {
    match item.get("type").and_then(|value| value.as_str()) {
        Some("message") => output_texts_from_content_array(item),
        Some("output_text") => item
            .get("text")
            .and_then(|value| value.as_str())
            .filter(|text| !text.is_empty())
            .map(|text| vec![text.to_string()])
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn output_texts_from_content_array(item: &serde_json::Value) -> Vec<String> {
    let Some(content) = item.get("content").and_then(|value| value.as_array()) else {
        return Vec::new();
    };

    content
        .iter()
        .filter(|content_item| {
            content_item.get("type").and_then(|value| value.as_str()) == Some("output_text")
        })
        .filter_map(|content_item| content_item.get("text").and_then(|value| value.as_str()))
        .filter(|text| !text.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn extract_last_text_from_value(value: &serde_json::Value, last: &mut Option<String>) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::String(text)) = map.get("text") {
                *last = Some(text.clone());
            }
            if let Some(serde_json::Value::String(delta)) = map.get("delta") {
                *last = Some(delta.clone());
            }
            if let Some(content) = map.get("content") {
                extract_last_text_from_value(content, last);
            }
            for (key, child) in map {
                if key == "text" || key == "delta" || key == "content" {
                    continue;
                }
                extract_last_text_from_value(child, last);
            }
        }
        serde_json::Value::Array(items) => {
            for child in items {
                extract_last_text_from_value(child, last);
            }
        }
        serde_json::Value::String(_)
        | serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::{
        extract_text_from_data, message_item_output_texts, parse_output_item_message_texts,
    };

    #[test]
    fn plaintext_data_extracts_as_text() {
        assert_eq!(extract_text_from_data("hello").as_deref(), Some("hello"));
    }

    #[test]
    fn json_data_extracts_text_field() {
        assert_eq!(
            extract_text_from_data(r#"{"text":"hi"}"#).as_deref(),
            Some("hi")
        );
    }

    #[test]
    fn message_item_extracts_output_text_content() {
        let item = serde_json::json!({
            "type": "message",
            "content": [
                { "type": "output_text", "text": "hello" },
                { "type": "input_text", "text": "ignored" },
                { "type": "output_text", "text": " world" }
            ]
        });

        assert_eq!(
            message_item_output_texts(&item),
            vec!["hello".to_string(), " world".to_string()]
        );
    }

    #[test]
    fn output_item_event_extracts_message_texts() {
        let data = r#"{"type":"response.output_item.done","item":{"type":"message","content":[{"type":"output_text","text":"done text"}]}}"#;
        assert_eq!(
            parse_output_item_message_texts("response.output_item.done", data),
            vec!["done text".to_string()]
        );
    }
}
