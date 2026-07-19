#![forbid(unsafe_code)]

pub fn apply_openai_strict_schema_gate(body: &mut serde_json::Value) {
    normalize_text_format_schema(body);
    normalize_tool_parameter_schemas(body);
}

#[must_use]
pub fn normalize_openai_strict_response_schema(schema: &serde_json::Value) -> serde_json::Value {
    let mut schema = schema.clone();
    normalize_openai_strict_schema_value(&mut schema);
    schema
}

fn normalize_text_format_schema(body: &mut serde_json::Value) {
    let Some(schema) = body
        .get_mut("text")
        .and_then(|text| text.get_mut("format"))
        .and_then(|format| format.get_mut("schema"))
    else {
        return;
    };

    normalize_openai_strict_schema_value(schema);
}

fn normalize_tool_parameter_schemas(body: &mut serde_json::Value) {
    let Some(tools) = body
        .get_mut("tools")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return;
    };

    for tool in tools {
        if let Some(parameters) = tool.get_mut("parameters") {
            normalize_openai_strict_schema_value(parameters);
        }

        if let Some(parameters) = tool
            .get_mut("function")
            .and_then(|function| function.get_mut("parameters"))
        {
            normalize_openai_strict_schema_value(parameters);
        }
    }
}

fn normalize_openai_strict_schema_value(schema: &mut serde_json::Value) {
    let Some(obj) = schema.as_object_mut() else {
        return;
    };

    if is_object_schema(obj) {
        normalize_openai_strict_object_schema(obj);
    }

    for key in [
        "items",
        "additionalProperties",
        "contains",
        "not",
        "if",
        "then",
        "else",
    ] {
        if let Some(value) = obj.get_mut(key) {
            normalize_openai_strict_schema_value(value);
        }
    }

    for key in ["anyOf", "oneOf", "allOf"] {
        if let Some(values) = obj.get_mut(key).and_then(serde_json::Value::as_array_mut) {
            for value in values {
                normalize_openai_strict_schema_value(value);
            }
        }
    }

    for key in ["$defs", "definitions"] {
        if let Some(defs) = obj.get_mut(key).and_then(serde_json::Value::as_object_mut) {
            for value in defs.values_mut() {
                normalize_openai_strict_schema_value(value);
            }
        }
    }
}

fn normalize_openai_strict_object_schema(obj: &mut serde_json::Map<String, serde_json::Value>) {
    obj.insert(
        "additionalProperties".to_string(),
        serde_json::Value::Bool(false),
    );

    let original_required = obj
        .get("required")
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(ToString::to_string)
                .collect::<std::collections::BTreeSet<_>>()
        })
        .unwrap_or_default();

    let properties = obj
        .entry("properties")
        .or_insert_with(|| serde_json::json!({}));

    let Some(properties) = properties.as_object_mut() else {
        return;
    };

    let mut property_names = properties.keys().cloned().collect::<Vec<_>>();
    property_names.sort();
    for property_name in &property_names {
        let Some(property) = properties.get_mut(property_name) else {
            continue;
        };
        normalize_openai_strict_schema_value(property);
        if !original_required.contains(property_name) {
            make_schema_nullable(property);
        }
    }

    obj.insert(
        "required".to_string(),
        serde_json::Value::Array(
            property_names
                .into_iter()
                .map(serde_json::Value::String)
                .collect(),
        ),
    );
}

fn is_object_schema(obj: &serde_json::Map<String, serde_json::Value>) -> bool {
    obj.get("properties").is_some()
        || obj
            .get("type")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|ty| ty == "object")
}

fn make_schema_nullable(schema: &mut serde_json::Value) {
    let Some(obj) = schema.as_object_mut() else {
        return;
    };

    match obj.get_mut("type") {
        Some(serde_json::Value::String(ty)) if ty != "null" => {
            let ty = ty.clone();
            obj.insert(
                "type".to_string(),
                serde_json::Value::Array(vec![
                    serde_json::Value::String(ty),
                    serde_json::Value::String("null".to_string()),
                ]),
            );
        }
        Some(serde_json::Value::Array(types)) => {
            if !types.iter().any(|value| value.as_str() == Some("null")) {
                types.push(serde_json::Value::String("null".to_string()));
            }
        }
        Some(_) => {}
        None => {
            let existing = serde_json::Value::Object(obj.clone());
            obj.clear();
            obj.insert(
                "anyOf".to_string(),
                serde_json::Value::Array(vec![
                    existing,
                    serde_json::json!({
                        "type": "null"
                    }),
                ]),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_normalizes_text_format_schema_optional_fields() {
        let mut body = serde_json::json!({
            "text": {
                "format": {
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
                }
            }
        });

        apply_openai_strict_schema_gate(&mut body);

        let schema = &body["text"]["format"]["schema"];
        assert_eq!(
            schema["required"],
            serde_json::json!(["impossible", "ok", "reason"])
        );
        assert_eq!(
            schema["properties"]["impossible"]["type"],
            serde_json::json!(["boolean", "null"])
        );
        assert_eq!(schema["additionalProperties"], false);
    }

    #[test]
    fn gate_normalizes_tool_parameter_schemas() {
        let mut body = serde_json::json!({
            "tools": [{
                "type": "function",
                "name": "Stop",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "decision": { "type": "string" },
                        "reason": { "type": "string" }
                    },
                    "required": ["decision"]
                }
            }]
        });

        apply_openai_strict_schema_gate(&mut body);

        let parameters = &body["tools"][0]["parameters"];
        assert_eq!(
            parameters["required"],
            serde_json::json!(["decision", "reason"])
        );
        assert_eq!(
            parameters["properties"]["reason"]["type"],
            serde_json::json!(["string", "null"])
        );
        assert_eq!(parameters["additionalProperties"], false);
    }
}
