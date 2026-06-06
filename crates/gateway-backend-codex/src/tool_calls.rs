#![forbid(unsafe_code)]

use crate::types::{CodexToolCall, CodexToolCallKind};

#[must_use]
pub fn parse_output_item_tool_call(event_name: &str, data: &str) -> Option<CodexToolCall> {
    if event_name != "response.output_item.done" && event_name != "response.output_item.added" {
        return None;
    }
    let value = serde_json::from_str::<serde_json::Value>(data).ok()?;
    let item = value
        .get("item")
        .cloned()
        .or_else(|| value.get("response").and_then(|r| r.get("item")).cloned())?;
    parse_tool_call_item(&item)
}

#[must_use]
pub fn parse_tool_call_item(item: &serde_json::Value) -> Option<CodexToolCall> {
    match item.get("type").and_then(|v| v.as_str())? {
        "function_call" => parse_function_call(item),
        "custom_tool_call" => parse_custom_tool_call(item),
        "tool_search_call" => parse_tool_search_call(item),
        "local_shell_call" => parse_local_shell_call(item),
        _ => None,
    }
}

fn parse_function_call(item: &serde_json::Value) -> Option<CodexToolCall> {
    let call_id = item.get("call_id")?.as_str()?.to_string();
    let name = item.get("name")?.as_str()?.to_string();
    let raw_arguments = item
        .get("arguments")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    Some(CodexToolCall {
        call_id,
        name,
        arguments: normalize_json_object_string(raw_arguments, "arguments"),
        kind: CodexToolCallKind::Function,
    })
}

fn parse_custom_tool_call(item: &serde_json::Value) -> Option<CodexToolCall> {
    let call_id = item.get("call_id")?.as_str()?.to_string();
    let name = item.get("name")?.as_str()?.to_string();
    let input = item
        .get("input")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    Some(CodexToolCall {
        call_id,
        name,
        arguments: serde_json::to_string(&serde_json::json!({ "input": input }))
            .unwrap_or_else(|_| "{}".to_string()),
        kind: CodexToolCallKind::Custom,
    })
}

fn parse_tool_search_call(item: &serde_json::Value) -> Option<CodexToolCall> {
    let call_id = item.get("call_id")?.as_str()?.to_string();
    let arguments = item
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    Some(CodexToolCall {
        call_id,
        name: "tool_search".to_string(),
        arguments: normalize_json_value_object_string(&arguments, "arguments"),
        kind: CodexToolCallKind::ToolSearch,
    })
}

fn parse_local_shell_call(item: &serde_json::Value) -> Option<CodexToolCall> {
    let call_id = item.get("call_id")?.as_str()?.to_string();
    let mut args = serde_json::Map::new();
    if let Some(status) = item.get("status").and_then(|v| v.as_str()) {
        args.insert(
            "status".to_string(),
            serde_json::Value::String(status.to_string()),
        );
    }
    if let Some(action) = item.get("action") {
        args.insert("action".to_string(), action.clone());
    }
    Some(CodexToolCall {
        call_id,
        name: "local_shell".to_string(),
        arguments: serde_json::to_string(&serde_json::Value::Object(args))
            .unwrap_or_else(|_| "{}".to_string()),
        kind: CodexToolCallKind::LocalShell,
    })
}

#[must_use]
pub fn normalize_json_object_string(raw: &str, fallback_field: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "{}".to_string();
    }
    match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(value) => normalize_json_value_object_string(&value, fallback_field),
        Err(_) => serde_json::to_string(&serde_json::json!({ fallback_field: raw }))
            .unwrap_or_else(|_| "{}".to_string()),
    }
}

#[must_use]
pub fn normalize_json_value_object_string(
    value: &serde_json::Value,
    fallback_field: &str,
) -> String {
    let object_value = if value.is_object() {
        value.clone()
    } else {
        serde_json::json!({ fallback_field: value })
    };
    serde_json::to_string(&object_value).unwrap_or_else(|_| "{}".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_function_call_arguments() {
        let data = r#"{"type":"response.output_item.done","item":{"type":"function_call","call_id":"call_1","name":"Read","arguments":"{\"file_path\":\"/tmp/a.txt\"}"}}"#;
        let call = parse_output_item_tool_call("response.output_item.done", data).unwrap();
        assert_eq!(call.kind, CodexToolCallKind::Function);
        assert_eq!(call.name, "Read");
        assert_eq!(call.arguments, r#"{"file_path":"/tmp/a.txt"}"#);
    }

    #[test]
    fn parses_custom_tool_call_as_object_input() {
        let data = r#"{"type":"response.output_item.done","item":{"type":"custom_tool_call","call_id":"call_2","name":"apply_patch","input":"*** Begin Patch\n*** End Patch\n"}}"#;
        let call = parse_output_item_tool_call("response.output_item.done", data).unwrap();
        assert_eq!(call.kind, CodexToolCallKind::Custom);
        assert_eq!(call.name, "apply_patch");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&call.arguments).unwrap(),
            serde_json::json!({"input":"*** Begin Patch\n*** End Patch\n"})
        );
    }

    #[test]
    fn parses_tool_search_call_arguments() {
        let data = r#"{"type":"response.output_item.done","item":{"type":"tool_search_call","call_id":"call_3","execution":"client","arguments":{"query":"Read"}}}"#;
        let call = parse_output_item_tool_call("response.output_item.done", data).unwrap();
        assert_eq!(call.kind, CodexToolCallKind::ToolSearch);
        assert_eq!(call.name, "tool_search");
        assert_eq!(call.arguments, r#"{"query":"Read"}"#);
    }

    #[test]
    fn parses_local_shell_call_action() {
        let data = r#"{"type":"response.output_item.done","item":{"type":"local_shell_call","call_id":"call_4","status":"completed","action":{"type":"exec","command":["echo","hi"]}}}"#;
        let call = parse_output_item_tool_call("response.output_item.done", data).unwrap();
        assert_eq!(call.kind, CodexToolCallKind::LocalShell);
        assert_eq!(call.name, "local_shell");
        let args = serde_json::from_str::<serde_json::Value>(&call.arguments).unwrap();
        assert_eq!(args["status"], "completed");
        assert_eq!(args["action"]["command"][0], "echo");
    }

    #[test]
    fn hosted_web_search_call_is_not_client_tool_call() {
        let data = r#"{"type":"response.output_item.done","item":{"type":"web_search_call","id":"ws_1","status":"completed","action":{"type":"search","query":"rust release"}}}"#;
        let call = parse_output_item_tool_call("response.output_item.done", data);
        assert!(call.is_none());
    }
}
