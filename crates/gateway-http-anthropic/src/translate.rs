#![forbid(unsafe_code)]

use crate::types::{
    AnthropicContent, AnthropicContentBlock, AnthropicMessage, AnthropicMessagesRequest,
    AnthropicSystemBlock, AnthropicToolDefinition,
};

pub struct TranslateResult {
    pub instructions: String,
    pub input: Vec<serde_json::Value>,
    pub tools: Vec<serde_json::Value>,
    pub tool_choice: String,
    pub parallel_tool_calls: bool,
    pub text: Option<serde_json::Value>,
    pub reasoning: Option<serde_json::Value>,
    pub include: Vec<String>,
}

pub fn translate_request(req: &AnthropicMessagesRequest) -> Result<TranslateResult, String> {
    let instructions = extract_system_text(&req.system).unwrap_or_default();
    let input = translate_messages_to_backend_items(&req.messages)?;
    let tools = translate_tools(&req.tools)?;
    let tool_choice = translate_tool_choice(req.tool_choice.as_ref());
    let text = translate_output_config(req.output_config.as_ref());

    Ok(TranslateResult {
        instructions,
        input,
        tools,
        tool_choice,
        parallel_tool_calls: true,
        text,
        reasoning: None,
        include: Vec::new(),
    })
}

pub fn extract_system_text(system: &[AnthropicSystemBlock]) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for block in system {
        if block.block_type != "text" {
            continue;
        }
        let Some(text) = block.text.as_deref() else {
            continue;
        };
        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }
        parts.push(trimmed);
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
    }
}

fn translate_messages_to_backend_items(
    messages: &[AnthropicMessage],
) -> Result<Vec<serde_json::Value>, String> {
    let mut items: Vec<serde_json::Value> = Vec::new();
    for msg in messages {
        let role = msg.role.as_str();
        match &msg.content {
            AnthropicContent::Text(t) => {
                if !t.trim().is_empty() {
                    items.push(serde_json::json!({
                        "type": "message",
                        "role": role,
                        "content": [content_item_for_role(role, t)],
                    }));
                }
            }
            AnthropicContent::Blocks(blocks) => {
                // Split blocks into message content vs tool-result items.
                let mut message_content: Vec<serde_json::Value> = Vec::new();
                for b in blocks {
                    let _unknown_field_count = b.extra.len();
                    match b.block_type.as_str() {
                        "text" => {
                            if let Some(text) = b.text.as_deref()
                                && !text.trim().is_empty()
                            {
                                message_content.push(content_item_for_role(role, text));
                            }
                        }
                        "image" => {
                            if role != "user" {
                                continue;
                            }
                            if let Some(img) = image_content_item(b) {
                                message_content.push(img);
                            }
                        }
                        "tool_result" => {
                            // tool_result is a separate ResponseItem in Codex protocol.
                            if let Some(item) = tool_result_item(b) {
                                items.push(item);
                            }
                        }
                        "tool_use" => {
                            // If the client sends a tool_use (e.g., replay/history), preserve it.
                            if let Some(item) = tool_use_item(b)? {
                                items.push(item);
                            }
                        }
                        // Preserve unknown blocks by dropping them (for now) with a hard error only
                        // when the block is structurally required.
                        _ => {}
                    }
                }
                if !message_content.is_empty() {
                    items.push(serde_json::json!({
                        "type": "message",
                        "role": role,
                        "content": message_content,
                    }));
                }
            }
        }
    }
    Ok(items)
}

fn content_item_for_role(role: &str, text: &str) -> serde_json::Value {
    // Codex protocol uses `input_text` for user and `output_text` for assistant.
    let item_type = if role == "assistant" {
        "output_text"
    } else {
        "input_text"
    };
    serde_json::json!({ "type": item_type, "text": text })
}

fn image_content_item(block: &AnthropicContentBlock) -> Option<serde_json::Value> {
    let Some(source) = &block.source else {
        return None;
    };
    if source.source_type != "base64" {
        return None;
    }
    let media_type = source.media_type.as_deref()?;
    let data = source.data.as_deref()?;
    let _unknown_source_field_count = source.extra.len();
    let url = format!("data:{media_type};base64,{data}");
    Some(serde_json::json!({ "type": "input_image", "image_url": url }))
}

fn tool_use_item(block: &AnthropicContentBlock) -> Result<Option<serde_json::Value>, String> {
    let Some(call_id) = block.id.as_deref() else {
        return Ok(None);
    };
    let Some(name) = block.name.as_deref() else {
        return Ok(None);
    };
    let input = block
        .input
        .clone()
        .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
    let arguments = serde_json::to_string(&input)
        .map_err(|e| format!("tool_use.input must be JSON-serializable: {e}"))?;

    Ok(Some(serde_json::json!({
        "type": "function_call",
        "name": name,
        "arguments": arguments,
        "call_id": call_id
    })))
}

fn tool_result_item(block: &AnthropicContentBlock) -> Option<serde_json::Value> {
    let call_id = block.tool_use_id.as_deref()?;
    let _is_error = block.is_error.unwrap_or(false);
    let output_text = extract_tool_result_text(block);
    Some(serde_json::json!({
        "type": "function_call_output",
        "call_id": call_id,
        "output": { "content": output_text }
    }))
}

fn extract_tool_result_text(block: &AnthropicContentBlock) -> String {
    let Some(content) = &block.content else {
        // Some clients might encode tool_result text in `text`.
        if let Some(text) = block.text.as_deref() {
            return text.to_string();
        }
        return String::new();
    };

    match content {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(items) => {
            let mut out: Vec<String> = Vec::new();
            for item in items {
                if let Some(s) = item.get("text").and_then(|v| v.as_str())
                    && !s.trim().is_empty()
                {
                    out.push(s.to_string());
                }
            }
            out.join("\n")
        }
        _ => String::new(),
    }
}

fn translate_tools(tools: &[AnthropicToolDefinition]) -> Result<Vec<serde_json::Value>, String> {
    let mut out = Vec::with_capacity(tools.len());
    for t in tools {
        let parameters = normalize_json_schema_parameters(&t.input_schema)?;
        out.push(serde_json::json!({
            "type": "function",
            "name": t.name,
            "description": t.description,
            "defer_loading": true,
            "parameters": parameters
        }));
    }
    Ok(out)
}

fn normalize_json_schema_parameters(
    schema: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    // Safe subset: object/properties/required/additionalProperties.
    let obj = schema
        .as_object()
        .ok_or("tool input_schema must be a JSON object")?;
    let ty = obj.get("type").and_then(|v| v.as_str()).unwrap_or("object");
    if ty != "object" {
        return Err("tool input_schema.type must be \"object\"".to_string());
    }
    let properties = obj
        .get("properties")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let required = obj
        .get("required")
        .cloned()
        .unwrap_or_else(|| serde_json::json!([]));
    let additional = obj
        .get("additionalProperties")
        .cloned()
        .unwrap_or(serde_json::Value::Bool(false));

    Ok(serde_json::json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": additional
    }))
}

fn translate_tool_choice(tool_choice: Option<&serde_json::Value>) -> String {
    // Best-effort: if absent, default to "auto".
    let Some(v) = tool_choice else {
        return "auto".to_string();
    };
    if let Some(s) = v.as_str() {
        return s.to_string();
    }
    if let Some(obj) = v.as_object() {
        if let Some(serde_json::Value::String(ty)) = obj.get("type") {
            if ty == "auto" {
                return "auto".to_string();
            }
            if ty == "any" {
                return "auto".to_string();
            }
        }
        if let Some(serde_json::Value::String(name)) = obj.get("name") {
            // Some Anthropic tool_choice variants specify a tool by name.
            return name.clone();
        }
    }
    "auto".to_string()
}

fn translate_output_config(
    output_config: Option<&crate::types::AnthropicOutputConfig>,
) -> Option<serde_json::Value> {
    let cfg = output_config?;
    let format = cfg.format.as_ref()?;
    let format_type = format.get("type").and_then(|v| v.as_str())?;
    if format_type != "json_schema" {
        return None;
    }
    let schema = format
        .get("schema")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    Some(serde_json::json!({
        "format": {
            "type": "json_schema",
            "strict": true,
            "schema": schema,
            "name": "anthropic_output_config"
        }
    }))
}
