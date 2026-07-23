#![forbid(unsafe_code)]

use crate::types::AnthropicOutputConfig;

pub(crate) fn structured_output_schema_from_config(
    output_config: Option<&AnthropicOutputConfig>,
) -> Option<serde_json::Value> {
    output_config
        .and_then(|config| config.format.as_ref())
        .filter(|format| format.get("type").and_then(|value| value.as_str()) == Some("json_schema"))
        .and_then(|format| format.get("schema"))
        .cloned()
}

pub(crate) fn cleanup_structured_output_text_for_anthropic(
    output_config: Option<&AnthropicOutputConfig>,
    text: &str,
) -> String {
    let schema = structured_output_schema_from_config(output_config);
    cleanup_structured_output_text_with_schema(schema.as_ref(), text)
}

pub(crate) fn cleanup_structured_output_text_with_schema(
    schema: Option<&serde_json::Value>,
    text: &str,
) -> String {
    let Some(schema) = schema else {
        return text.to_string();
    };

    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(text) else {
        return text.to_string();
    };

    remove_null_optional_fields(&mut value, schema);
    serde_json::to_string(&value).unwrap_or_else(|_| text.to_string())
}

pub(crate) fn sanitize_anthropic_response_value(mut value: serde_json::Value) -> serde_json::Value {
    remove_null_fields_and_empty_text_blocks(&mut value);
    value
}

pub(crate) fn sanitize_anthropic_response_text(text: String) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return text;
    };
    serde_json::to_string(&sanitize_anthropic_response_value(value)).unwrap_or(text)
}

fn remove_null_fields_and_empty_text_blocks(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(object) => {
            for nested in object.values_mut() {
                remove_null_fields_and_empty_text_blocks(nested);
            }
            object.retain(|_, value| !value.is_null());
            if let Some(content) = object.get_mut("content")
                && let Some(blocks) = content.as_array_mut()
            {
                blocks.retain(|block| {
                    block.get("type").and_then(serde_json::Value::as_str) != Some("text")
                        || block
                            .get("text")
                            .and_then(serde_json::Value::as_str)
                            .is_some_and(|text| !text.trim().is_empty())
                });
            }
        }
        serde_json::Value::Array(values) => {
            for nested in values {
                remove_null_fields_and_empty_text_blocks(nested);
            }
        }
        _ => {}
    }
}

fn remove_null_optional_fields(value: &mut serde_json::Value, schema: &serde_json::Value) {
    if let (Some(value_obj), Some(schema_obj)) = (value.as_object_mut(), schema.as_object()) {
        let required = schema_obj
            .get("required")
            .and_then(serde_json::Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .collect::<std::collections::BTreeSet<_>>()
            })
            .unwrap_or_default();

        if let Some(properties) = schema_obj
            .get("properties")
            .and_then(serde_json::Value::as_object)
        {
            let property_names = properties.keys().cloned().collect::<Vec<_>>();
            for property_name in property_names {
                let Some(property_schema) = properties.get(&property_name) else {
                    continue;
                };
                let Some(property_value) = value_obj.get_mut(&property_name) else {
                    continue;
                };
                if property_value.is_null() && !required.contains(property_name.as_str()) {
                    value_obj.remove(&property_name);
                    continue;
                }
                remove_null_optional_fields(property_value, property_schema);
            }
        }
    }

    if let (Some(values), Some(items_schema)) = (value.as_array_mut(), schema.get("items")) {
        for item in values {
            remove_null_optional_fields(item, items_schema);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_null_optional_fields_before_anthropic_response() {
        let output_config = AnthropicOutputConfig {
            effort: None,
            format: Some(serde_json::json!({
                "type": "json_schema",
                "schema": {
                    "type": "object",
                    "properties": {
                        "ok": { "type": "boolean" },
                        "reason": { "type": "string" },
                        "impossible": { "type": "boolean" }
                    },
                    "required": ["ok", "reason"]
                }
            })),
        };

        let cleaned = cleanup_structured_output_text_for_anthropic(
            Some(&output_config),
            r#"{"ok":true,"reason":"continuing","impossible":null}"#,
        );
        let value = serde_json::from_str::<serde_json::Value>(&cleaned).expect("json");

        assert_eq!(
            value,
            serde_json::json!({
                "ok": true,
                "reason": "continuing"
            })
        );
    }

    #[test]
    fn sanitize_anthropic_response_removes_nulls_and_empty_text_blocks() {
        let value = sanitize_anthropic_response_value(serde_json::json!({
            "type": "message",
            "stop_sequence": null,
            "content": [
                { "type": "text", "text": "" },
                { "type": "text", "text": "hello" },
                { "type": "tool_use", "id": "tool_1", "input": null }
            ],
            "nested": { "impossible": null, "ok": true }
        }));

        assert_eq!(
            value,
            serde_json::json!({
                "type": "message",
                "content": [
                    { "type": "text", "text": "hello" },
                    { "type": "tool_use", "id": "tool_1" }
                ],
                "nested": { "ok": true }
            })
        );
    }

    #[test]
    fn keeps_required_nulls_and_recurses_into_arrays() {
        let output_config = AnthropicOutputConfig {
            effort: None,
            format: Some(serde_json::json!({
                "type": "json_schema",
                "schema": {
                    "type": "object",
                    "properties": {
                        "status": { "type": "string" },
                        "items": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "name": { "type": "string" },
                                    "optional_note": { "type": "string" }
                                },
                                "required": ["name"]
                            }
                        },
                        "required_nullable": { "type": ["string", "null"] }
                    },
                    "required": ["status", "items", "required_nullable"]
                }
            })),
        };

        let cleaned = cleanup_structured_output_text_for_anthropic(
            Some(&output_config),
            r#"{"status":"ok","items":[{"name":"a","optional_note":null}],"required_nullable":null}"#,
        );
        let value = serde_json::from_str::<serde_json::Value>(&cleaned).expect("json");

        assert_eq!(
            value,
            serde_json::json!({
                "status": "ok",
                "items": [{ "name": "a" }],
                "required_nullable": null
            })
        );
    }
}
