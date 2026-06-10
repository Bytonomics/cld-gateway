#![forbid(unsafe_code)]

use crate::claude_code_context::normalize_claude_code_context;
use crate::types::{
    AnthropicContent, AnthropicContentBlock, AnthropicMessage, AnthropicMessagesRequest,
    AnthropicSystemBlock, AnthropicToolDefinition,
};

use gateway_backend_codex::types::CodexToolCallKind;
use gateway_core::config::ClaudeCodeWorkflowConfig;
use std::collections::HashMap;

const WEB_SEARCH_SOURCES_INCLUDE: &str = "web_search_call.action.sources";
const ANTHROPIC_WEB_SEARCH_TYPE: &str = "web_search_20250305";
const OPENAI_WEB_SEARCH_TYPE: &str = "web_search";

pub struct TranslateResult {
    pub instructions: String,
    pub input: Vec<serde_json::Value>,
    pub tools: Vec<serde_json::Value>,
    pub tool_choice: String,
    pub parallel_tool_calls: bool,
    pub text: Option<serde_json::Value>,
    pub reasoning: Option<serde_json::Value>,
    pub include: Vec<String>,
    pub client_metadata: Option<HashMap<String, String>>,
}

struct TranslatedTools {
    tools: Vec<serde_json::Value>,
    include: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ToolTranslationContext {
    tool_kinds_by_call_id: HashMap<String, CodexToolCallKind>,
    claude_code_config: ClaudeCodeWorkflowConfig,
}

impl ToolTranslationContext {
    #[must_use]
    pub fn new(tool_kinds_by_call_id: HashMap<String, CodexToolCallKind>) -> Self {
        Self {
            tool_kinds_by_call_id,
            claude_code_config: ClaudeCodeWorkflowConfig::default(),
        }
    }

    #[must_use]
    pub fn with_claude_code_config(mut self, config: ClaudeCodeWorkflowConfig) -> Self {
        self.claude_code_config = config;
        self
    }

    fn kind_for_call(&self, call_id: &str) -> CodexToolCallKind {
        self.tool_kinds_by_call_id
            .get(call_id)
            .copied()
            .unwrap_or(CodexToolCallKind::Function)
    }
}

pub fn translate_request_with_context(
    req: &AnthropicMessagesRequest,
    tool_context: &ToolTranslationContext,
) -> Result<TranslateResult, String> {
    let normalized = normalize_claude_code_context(&req.messages, &tool_context.claude_code_config);
    let base_instructions = extract_system_text(&req.system)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "You are a helpful assistant.".to_string());
    let instructions = if normalized.instruction_fragments.is_empty() {
        base_instructions
    } else {
        let mut instructions = normalized.instruction_fragments.join("\n\n");
        instructions.push_str("\n\n");
        instructions.push_str(&base_instructions);
        instructions
    };
    let input = translate_messages_to_backend_items(&normalized.messages, tool_context)?;
    let hosted_web_search = req
        .tools
        .iter()
        .any(|tool| tool.tool_type.as_deref() == Some(ANTHROPIC_WEB_SEARCH_TYPE));
    let translated_tools = translate_tools(&req.tools)?;
    let tool_choice = if hosted_web_search {
        "required".to_string()
    } else {
        translate_tool_choice(req.tool_choice.as_ref())
    };
    let text = translate_output_config(req.output_config.as_ref());

    let mut client_metadata: HashMap<String, String> = HashMap::new();
    let reasoning =
        translate_effort_to_backend_reasoning(req.output_config.as_ref(), &mut client_metadata);
    if let Some(max_tokens) = req.max_tokens {
        client_metadata.insert("anthropic_max_tokens".to_string(), max_tokens.to_string());
    }
    if let Some(top_k) = req.top_k {
        client_metadata.insert("anthropic_top_k".to_string(), top_k.to_string());
    }
    if let Some(temperature) = req.temperature {
        client_metadata.insert("anthropic_temperature".to_string(), temperature.to_string());
    }
    if let Some(top_p) = req.top_p {
        client_metadata.insert("anthropic_top_p".to_string(), top_p.to_string());
    }
    if let Some(metadata) = req.metadata.as_ref() {
        let encoded = serde_json::to_string(metadata)
            .map_err(|e| format!("metadata must be JSON-serializable: {e}"))?;
        client_metadata.insert("anthropic_metadata".to_string(), encoded);
    }
    client_metadata.extend(normalized.client_metadata);
    let client_metadata = (!client_metadata.is_empty()).then_some(client_metadata);

    Ok(TranslateResult {
        instructions,
        input,
        tools: translated_tools.tools,
        tool_choice,
        parallel_tool_calls: true,
        text,
        reasoning,
        include: translated_tools.include,
        client_metadata,
    })
}

fn translate_effort_to_backend_reasoning(
    output_config: Option<&crate::types::AnthropicOutputConfig>,
    client_metadata: &mut HashMap<String, String>,
) -> Option<serde_json::Value> {
    let cfg = output_config?;
    let effort = cfg.effort.as_deref()?;
    let normalized = effort.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return None;
    }

    client_metadata.insert("anthropic_effort".to_string(), normalized.clone());

    let mapped = match normalized.as_str() {
        // 1:1 overlap (and best-effort pass-through for values known to be accepted by the backend).
        "low" | "medium" | "high" | "none" | "minimal" => normalized,
        // Claude Code "max"/"xhigh" are not guaranteed backend-accepted; map deterministically to avoid 400s.
        "max" | "xhigh" => "high".to_string(),
        _ => {
            client_metadata.insert("anthropic_effort_unmapped".to_string(), normalized);
            return None;
        }
    };

    Some(serde_json::json!({ "effort": mapped }))
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
    tool_context: &ToolTranslationContext,
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
                            if let Some(item) = tool_result_item(b, tool_context) {
                                items.push(item);
                            }
                        }
                        "tool_use" => {
                            // If the client sends a tool_use (e.g., replay/history), preserve it.
                            if let Some(item) = tool_use_item(b, tool_context)? {
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

fn tool_use_item(
    block: &AnthropicContentBlock,
    tool_context: &ToolTranslationContext,
) -> Result<Option<serde_json::Value>, String> {
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

    let item = match tool_context.kind_for_call(call_id) {
        CodexToolCallKind::Function => serde_json::json!({
            "type": "function_call",
            "name": name,
            "arguments": arguments,
            "call_id": call_id
        }),
        CodexToolCallKind::Custom => serde_json::json!({
            "type": "custom_tool_call",
            "name": name,
            "input": custom_tool_input_text(&input),
            "call_id": call_id
        }),
        CodexToolCallKind::ToolSearch => serde_json::json!({
            "type": "tool_search_call",
            "call_id": call_id,
            "execution": "client",
            "arguments": input
        }),
        CodexToolCallKind::LocalShell => serde_json::json!({
            "type": "local_shell_call",
            "call_id": call_id,
            "status": input
                .get("status")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("completed"),
            "action": input
                .get("action")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}))
        }),
    };

    Ok(Some(item))
}

fn custom_tool_input_text(input: &serde_json::Value) -> String {
    input
        .get("input")
        .and_then(serde_json::Value::as_str)
        .map_or_else(
            || serde_json::to_string(input).unwrap_or_default(),
            str::to_string,
        )
}

fn tool_result_item(
    block: &AnthropicContentBlock,
    tool_context: &ToolTranslationContext,
) -> Option<serde_json::Value> {
    let call_id = block.tool_use_id.as_deref()?;
    let _is_error = block.is_error.unwrap_or(false);
    let kind = tool_context.kind_for_call(call_id);
    if kind == CodexToolCallKind::ToolSearch {
        return Some(tool_search_output_item(call_id, block));
    }

    let output = tool_result_output_value(block);
    Some(serde_json::json!({
        "type": kind.output_type(),
        "call_id": call_id,
        // Codex protocol wire format: `output` is either a plain string or an array of
        // structured content items ("content_items").
        "output": output
    }))
}

fn tool_search_output_item(call_id: &str, block: &AnthropicContentBlock) -> serde_json::Value {
    serde_json::json!({
        "type": CodexToolCallKind::ToolSearch.output_type(),
        "call_id": call_id,
        "status": "completed",
        "execution": "client",
        "tools": tool_search_tools(block)
    })
}

fn tool_search_tools(block: &AnthropicContentBlock) -> serde_json::Value {
    let Some(content) = block.content.as_ref() else {
        return serde_json::json!([]);
    };
    if let Some(tools) = content.get("tools").and_then(serde_json::Value::as_array) {
        return serde_json::Value::Array(tools.clone());
    }
    if let Some(array) = content.as_array() {
        return serde_json::Value::Array(array.clone());
    }
    serde_json::json!([])
}

fn tool_result_output_value(block: &AnthropicContentBlock) -> serde_json::Value {
    let Some(content) = &block.content else {
        // Some clients might encode tool_result text in `text`.
        if let Some(text) = block.text.as_deref() {
            return serde_json::Value::String(text.to_string());
        }
        return serde_json::Value::String(String::new());
    };

    match content {
        serde_json::Value::String(s) => serde_json::Value::String(s.clone()),
        serde_json::Value::Array(items) => {
            let mut out: Vec<serde_json::Value> = Vec::new();
            for item in items {
                let Some(obj) = item.as_object() else {
                    let encoded = serde_json::to_string(item).unwrap_or_default();
                    out.push(serde_json::json!({ "type": "input_text", "text": encoded }));
                    continue;
                };
                let item_type = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");
                match item_type {
                    "text" => {
                        if let Some(text) = obj.get("text").and_then(|v| v.as_str())
                            && !text.trim().is_empty()
                        {
                            out.push(serde_json::json!({ "type": "input_text", "text": text }));
                        }
                    }
                    "image" => {
                        // Try to preserve multimodal tool outputs as Responses-compatible content items.
                        let source = obj.get("source").and_then(|v| v.as_object());
                        let source_type = source
                            .and_then(|s| s.get("type"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        if source_type == "base64" {
                            let media_type = source
                                .and_then(|s| s.get("media_type"))
                                .and_then(|v| v.as_str());
                            let data = source.and_then(|s| s.get("data")).and_then(|v| v.as_str());
                            if let (Some(media_type), Some(data)) = (media_type, data) {
                                let url = format!("data:{media_type};base64,{data}");
                                out.push(serde_json::json!({
                                    "type": "input_image",
                                    "image_url": url
                                }));
                            }
                        }
                    }
                    _ => {
                        let encoded = serde_json::to_string(item).unwrap_or_default();
                        out.push(serde_json::json!({ "type": "input_text", "text": encoded }));
                    }
                }
            }
            if out.is_empty() {
                serde_json::Value::String(String::new())
            } else {
                serde_json::Value::Array(out)
            }
        }
        _ => serde_json::Value::String(String::new()),
    }
}

fn translate_tools(tools: &[AnthropicToolDefinition]) -> Result<TranslatedTools, String> {
    let mut out = Vec::with_capacity(tools.len());
    let mut include = Vec::new();
    for t in tools {
        match t.tool_type.as_deref() {
            Some(ANTHROPIC_WEB_SEARCH_TYPE) => {
                out.push(translate_hosted_web_search_tool(t)?);
                if !include
                    .iter()
                    .any(|field| field == WEB_SEARCH_SOURCES_INCLUDE)
                {
                    include.push(WEB_SEARCH_SOURCES_INCLUDE.to_string());
                }
            }
            Some(other) => {
                return Err(format!(
                    "unsupported Anthropic hosted tool type `{other}` for tool `{}`",
                    t.name
                ));
            }
            None => {
                let parameters = tool_schema_parameters_for_backend(&t.name, &t.input_schema)?;
                out.push(serde_json::json!({
                    "type": "function",
                    "name": t.name,
                    "description": t.description,
                    "parameters": parameters
                }));
            }
        }
    }
    Ok(TranslatedTools {
        tools: out,
        include,
    })
}

fn translate_hosted_web_search_tool(
    tool: &AnthropicToolDefinition,
) -> Result<serde_json::Value, String> {
    if tool.name != OPENAI_WEB_SEARCH_TYPE {
        return Err(format!(
            "Anthropic `{ANTHROPIC_WEB_SEARCH_TYPE}` tool must be named `{OPENAI_WEB_SEARCH_TYPE}`, got `{}`",
            tool.name
        ));
    }
    if !tool.input_schema.is_null() {
        return Err(format!(
            "Anthropic `{ANTHROPIC_WEB_SEARCH_TYPE}` tool must not include `input_schema`"
        ));
    }
    if !tool.blocked_domains.is_empty() {
        return Err(format!(
            "Anthropic `{ANTHROPIC_WEB_SEARCH_TYPE}` field `blocked_domains` is unsupported by OpenAI web_search translation; remove blocked_domains or use allowed_domains"
        ));
    }
    if !tool.extra.is_empty() {
        let fields = tool.extra.keys().cloned().collect::<Vec<_>>().join(", ");
        return Err(format!(
            "Anthropic `{ANTHROPIC_WEB_SEARCH_TYPE}` has unsupported field(s): {fields}"
        ));
    }
    if tool.max_uses == Some(0) {
        return Err(format!(
            "Anthropic `{ANTHROPIC_WEB_SEARCH_TYPE}` field `max_uses` must be greater than 0"
        ));
    }

    let mut obj = serde_json::Map::from_iter([
        (
            "type".to_string(),
            serde_json::Value::String(OPENAI_WEB_SEARCH_TYPE.to_string()),
        ),
        (
            "external_web_access".to_string(),
            serde_json::Value::Bool(true),
        ),
    ]);

    if !tool.allowed_domains.is_empty() {
        obj.insert(
            "filters".to_string(),
            serde_json::json!({ "allowed_domains": tool.allowed_domains.clone() }),
        );
    }

    Ok(serde_json::Value::Object(obj))
}

fn tool_schema_parameters_for_backend(
    tool_name: &str,
    schema: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let mut parameters = normalize_json_schema_parameters(schema)?;
    apply_backend_tool_schema_policies(tool_name, &mut parameters);
    Ok(parameters)
}

fn apply_backend_tool_schema_policies(tool_name: &str, parameters: &mut serde_json::Value) {
    if tool_name != "Agent" {
        return;
    }

    let Some(obj) = parameters.as_object_mut() else {
        return;
    };

    if let Some(properties) = obj
        .get_mut("properties")
        .and_then(serde_json::Value::as_object_mut)
    {
        properties.remove("isolation");
    }

    if let Some(required) = obj
        .get_mut("required")
        .and_then(serde_json::Value::as_array_mut)
    {
        required.retain(|field| field.as_str() != Some("isolation"));
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use gateway_backend_codex::types::CodexToolCallKind;
    use gateway_core::DEFAULT_BACKEND_MODEL;

    fn fixture(path: &str) -> String {
        let full = format!("{}/tests/fixtures/{}", env!("CARGO_MANIFEST_DIR"), path);
        std::fs::read_to_string(full)
            .expect("read fixture")
            .replace("__DEFAULT_BACKEND_MODEL__", DEFAULT_BACKEND_MODEL)
    }

    fn base_req() -> AnthropicMessagesRequest {
        AnthropicMessagesRequest {
            model: DEFAULT_BACKEND_MODEL.to_string(),
            messages: Vec::new(),
            system: Vec::new(),
            stream: false,
            stop_sequences: Vec::new(),
            max_tokens: None,
            temperature: None,
            top_p: None,
            top_k: None,
            metadata: None,
            tools: Vec::new(),
            tool_choice: None,
            thinking: None,
            context_management: None,
            output_config: None,
        }
    }

    fn context_with(call_id: &str, kind: CodexToolCallKind) -> ToolTranslationContext {
        ToolTranslationContext::new(HashMap::from([(call_id.to_string(), kind)]))
    }

    fn translate_request(req: &AnthropicMessagesRequest) -> Result<TranslateResult, String> {
        translate_request_with_context(req, &ToolTranslationContext::default())
    }

    fn serialized_input(translated: &TranslateResult) -> String {
        serde_json::to_string(&translated.input).expect("serialize input")
    }

    fn test_text_block(text: impl Into<String>) -> AnthropicContentBlock {
        AnthropicContentBlock {
            block_type: "text".to_string(),
            text: Some(text.into()),
            id: None,
            name: None,
            input: None,
            tool_use_id: None,
            content: None,
            is_error: None,
            source: None,
            extra: std::collections::BTreeMap::default(),
        }
    }

    #[test]
    fn defaults_instructions_when_system_empty() {
        let req = base_req();
        let translated = translate_request(&req).expect("translate");
        assert_eq!(translated.instructions, "You are a helpful assistant.");
    }

    #[test]
    fn latest_user_message_gets_priority_directive() {
        let mut req = base_req();
        req.messages.push(AnthropicMessage {
            role: "user".to_string(),
            content: AnthropicContent::Text("explain the current diff".to_string()),
        });

        let translated = translate_request(&req).expect("translate");
        let input = serialized_input(&translated);

        assert!(
            translated
                .instructions
                .starts_with("Everything before the latest user message or slash command")
        );
        assert!(input.contains("explain the current diff"));
    }

    #[test]
    fn latest_claude_code_slash_command_promotes_body_and_uses_args_as_input() {
        let mut req = base_req();
        req.messages.push(AnthropicMessage {
            role: "user".to_string(),
            content: AnthropicContent::Text(
                "<command-message>review_agent</command-message>\n\
                 <command-name>/review_agent</command-name>\n\
                 <command-args>verify if these tasks are implemented by another agent</command-args>\n\
                 another agent continued your session and implemented most of the tasks.\n\
                 Generate the report as 2 markdown tables.\n\
                 ARGUMENTS: verify if these tasks are implemented by another agent"
                    .to_string(),
            ),
        });

        let translated = translate_request(&req).expect("translate");
        let input = serialized_input(&translated);

        assert!(
            translated
                .instructions
                .starts_with("Everything before the latest user message or slash command")
        );
        assert!(
            translated
                .instructions
                .contains("implemented most of the tasks")
        );
        assert!(
            translated
                .instructions
                .contains("The slash command instructions below are complete")
        );
        assert!(input.contains("verify if these tasks are implemented by another agent"));
        assert!(!input.contains("implemented most of the tasks"));
        assert_eq!(
            translated
                .client_metadata
                .as_ref()
                .and_then(|metadata| metadata.get("claude_code_slash_command"))
                .map(String::as_str),
            Some("/review_agent")
        );
    }

    #[test]
    fn older_claude_code_slash_commands_are_historical_not_active() {
        let mut req = base_req();
        req.messages.push(AnthropicMessage {
            role: "user".to_string(),
            content: AnthropicContent::Text(
                "<command-message>review_agent</command-message>\n\
                 <command-name>/review_agent</command-name>\n\
                 <command-args>old review</command-args>\n\
                 OLD COMMAND BODY SHOULD NOT SURVIVE"
                    .to_string(),
            ),
        });
        req.messages.push(AnthropicMessage {
            role: "assistant".to_string(),
            content: AnthropicContent::Text("ack".to_string()),
        });
        req.messages.push(AnthropicMessage {
            role: "user".to_string(),
            content: AnthropicContent::Text("answer only the new question".to_string()),
        });

        let translated = translate_request(&req).expect("translate");
        let input = serialized_input(&translated);

        assert!(
            !translated
                .instructions
                .contains("OLD COMMAND BODY SHOULD NOT SURVIVE")
        );
        assert!(input.contains("Previous command context only"));
        assert!(input.contains("/review_agent"));
        assert!(input.contains("old review"));
        assert!(input.contains("OLD COMMAND BODY SHOULD NOT SURVIVE"));
        assert!(input.contains("answer only the new question"));
    }

    #[test]
    fn historical_slash_command_scrubbing_preserves_tool_results() {
        let mut req = base_req();
        req.messages.push(AnthropicMessage {
            role: "assistant".to_string(),
            content: AnthropicContent::Blocks(vec![AnthropicContentBlock {
                block_type: "tool_use".to_string(),
                text: None,
                id: Some("call_preserved".to_string()),
                name: Some("Bash".to_string()),
                input: Some(serde_json::json!({ "command": "echo hi" })),
                tool_use_id: None,
                content: None,
                is_error: None,
                source: None,
                extra: std::collections::BTreeMap::default(),
            }]),
        });
        req.messages.push(AnthropicMessage {
            role: "user".to_string(),
            content: AnthropicContent::Blocks(vec![
                AnthropicContentBlock {
                    block_type: "tool_result".to_string(),
                    text: None,
                    id: None,
                    name: None,
                    input: None,
                    tool_use_id: Some("call_preserved".to_string()),
                    content: Some(serde_json::json!("completed")),
                    is_error: None,
                    source: None,
                    extra: std::collections::BTreeMap::default(),
                },
                AnthropicContentBlock {
                    block_type: "text".to_string(),
                    text: Some(
                        "<command-name>/model</command-name>\n\
                         <command-message>model</command-message>\n\
                         <command-args></command-args>\n\
                         <local-command-stdout>Set model to gpt-5.4</local-command-stdout>"
                            .to_string(),
                    ),
                    id: None,
                    name: None,
                    input: None,
                    tool_use_id: None,
                    content: None,
                    is_error: None,
                    source: None,
                    extra: std::collections::BTreeMap::default(),
                },
            ]),
        });
        req.messages.push(AnthropicMessage {
            role: "user".to_string(),
            content: AnthropicContent::Text("answer the current question".to_string()),
        });

        let translated = translate_request(&req).expect("translate");
        let input = serialized_input(&translated);

        assert!(input.contains("\"type\":\"function_call\""));
        assert!(input.contains("\"type\":\"function_call_output\""));
        assert!(input.contains("\"call_id\":\"call_preserved\""));
        assert!(input.contains("answer the current question"));
    }

    #[test]
    fn active_claude_code_slash_command_does_not_promote_older_command() {
        let mut req = base_req();
        req.messages.push(AnthropicMessage {
            role: "user".to_string(),
            content: AnthropicContent::Text(
                "<command-message>old_command</command-message>\n\
                 <command-name>/old_command</command-name>\n\
                 <command-args>old args</command-args>\n\
                 OLD COMMAND BODY"
                    .to_string(),
            ),
        });
        req.messages.push(AnthropicMessage {
            role: "user".to_string(),
            content: AnthropicContent::Text(
                "<command-message>make_tasks</command-message>\n\
                 <command-name>/make-tasks-for-plan</command-name>\n\
                 <command-args>make tasks from the existing approved plan</command-args>\n\
                 Create tasks from the existing approved plan. Do not rewrite the plan."
                    .to_string(),
            ),
        });

        let translated = translate_request(&req).expect("translate");
        let input = serialized_input(&translated);

        assert!(translated.instructions.contains("Do not rewrite the plan"));
        assert!(input.contains("make tasks from the existing approved plan"));
        assert!(input.contains("Previous command context only"));
        assert!(input.contains("/old_command"));
        assert!(input.contains("OLD COMMAND BODY"));
    }

    #[test]
    fn active_slash_command_uses_latest_envelope_inside_same_user_message() {
        let mut req = base_req();
        req.messages.push(AnthropicMessage {
            role: "user".to_string(),
            content: AnthropicContent::Blocks(vec![
                AnthropicContentBlock {
                    block_type: "text".to_string(),
                    text: Some(
                        "<command-name>/skills</command-name>\n\
                         <command-message>skills</command-message>\n\
                         <command-args></command-args>"
                            .to_string(),
                    ),
                    id: None,
                    name: None,
                    input: None,
                    tool_use_id: None,
                    content: None,
                    is_error: None,
                    source: None,
                    extra: std::collections::BTreeMap::default(),
                },
                AnthropicContentBlock {
                    block_type: "text".to_string(),
                    text: Some(
                        "<local-command-stdout>Skills dialog dismissed</local-command-stdout>"
                            .to_string(),
                    ),
                    id: None,
                    name: None,
                    input: None,
                    tool_use_id: None,
                    content: None,
                    is_error: None,
                    source: None,
                    extra: std::collections::BTreeMap::default(),
                },
                AnthropicContentBlock {
                    block_type: "text".to_string(),
                    text: Some(
                        "<command-message>explain-feature</command-message>\n\
                         <command-name>/explain-feature</command-name>\n\
                         <command-args>explain the currently unstaged feature in this repo</command-args>"
                            .to_string(),
                    ),
                    id: None,
                    name: None,
                    input: None,
                    tool_use_id: None,
                    content: None,
                    is_error: None,
                    source: None,
                    extra: std::collections::BTreeMap::default(),
                },
                AnthropicContentBlock {
                    block_type: "text".to_string(),
                    text: Some(
                        "You are a code explainer for this repository.\n\
                         Do not dump everything at once."
                            .to_string(),
                    ),
                    id: None,
                    name: None,
                    input: None,
                    tool_use_id: None,
                    content: None,
                    is_error: None,
                    source: None,
                    extra: std::collections::BTreeMap::default(),
                },
            ]),
        });

        let translated = translate_request(&req).expect("translate");
        let input = serialized_input(&translated);

        assert!(!translated.input.is_empty());
        assert!(
            translated
                .instructions
                .contains("You are a code explainer for this repository.")
        );
        assert!(input.contains("explain the currently unstaged feature in this repo"));
        assert_eq!(
            translated
                .client_metadata
                .as_ref()
                .and_then(|metadata| metadata.get("claude_code_slash_command"))
                .map(String::as_str),
            Some("/explain-feature")
        );
    }

    #[test]
    fn local_only_branch_command_is_removed_before_latest_prompt() {
        let mut req = base_req();
        req.messages.push(AnthropicMessage {
            role: "user".to_string(),
            content: AnthropicContent::Blocks(vec![
                test_text_block(
                    "<system-reminder>\n\
                     SessionStart:resume hook success.\n\
                     </system-reminder>",
                ),
                test_text_block(
                    "<command-name>/branch</command-name>\n\
                     <command-message>branch</command-message>\n\
                     <command-args></command-args>",
                ),
                test_text_block(
                    "<local-command-stdout>Branched conversation. You are now in the branch.\n\
                     To resume the original: claude -r test-session-id</local-command-stdout>",
                ),
                test_text_block(
                    "Failure reason\n\
                     Resource handler returned message: \"Your service failed to create.\"\n\n\
                     The deployment just failed. Check the logs and tell me why it failed.",
                ),
            ]),
        });

        let translated = translate_request(&req).expect("translate");
        let input = serialized_input(&translated);
        let metadata = translated.client_metadata.as_ref().expect("metadata");

        assert!(
            translated
                .instructions
                .starts_with("Everything before the latest user message or slash command")
        );
        assert!(!translated.instructions.contains("/branch"));
        assert!(!translated.instructions.contains("Branched conversation"));
        assert!(input.contains("The deployment just failed"));
        assert!(!input.contains("/branch"));
        assert!(!input.contains("Branched conversation"));
        assert!(!input.contains("claude -r"));
        assert!(
            !metadata.contains_key("claude_code_slash_command"),
            "local-only commands must not be promoted as slash commands"
        );
        assert_eq!(
            metadata
                .get("gateway_local_only_commands")
                .map(String::as_str),
            Some("/branch")
        );
    }

    #[test]
    fn local_only_rename_command_is_removed_before_active_command() {
        let mut req = base_req();
        req.messages.push(AnthropicMessage {
            role: "user".to_string(),
            content: AnthropicContent::Blocks(vec![
                test_text_block(
                    "<command-name>/rename</command-name>\n\
                     <command-message>rename</command-message>\n\
                     <command-args>gateway-cli-release-distribution-plan</command-args>",
                ),
                test_text_block(
                    "<local-command-stdout>Session renamed to: gateway-cli-release-distribution-plan</local-command-stdout>",
                ),
                test_text_block(
                    "<command-message>review_agent</command-message>\n\
                     <command-name>/review_agent</command-name>\n\
                     <command-args>review the implementation</command-args>",
                ),
                test_text_block("Review the implementation and report issues."),
            ]),
        });

        let translated = translate_request(&req).expect("translate");
        let input = serialized_input(&translated);
        let metadata = translated.client_metadata.as_ref().expect("metadata");

        assert!(
            translated
                .instructions
                .contains("Review the implementation")
        );
        assert!(input.contains("review the implementation"));
        assert!(!translated.instructions.contains("/rename"));
        assert!(!translated.instructions.contains("Session renamed to:"));
        assert!(!input.contains("/rename"));
        assert!(!input.contains("Session renamed to:"));
        assert_eq!(
            metadata
                .get("gateway_local_only_commands")
                .map(String::as_str),
            Some("/rename")
        );
        assert_eq!(
            metadata
                .get("claude_code_slash_command")
                .map(String::as_str),
            Some("/review_agent")
        );
    }

    #[test]
    fn read_only_side_question_is_marked_without_using_literal_btw() {
        let mut req = base_req();
        req.messages.push(AnthropicMessage {
            role: "user".to_string(),
            content: AnthropicContent::Blocks(vec![
                test_text_block(
                    "<system-reminder>\n\
                     This is a side question from the user.\n\
                     You are a separate, lightweight agent spawned to answer this one question.\n\
                     The main agent is NOT interrupted by this request.\n\
                     </system-reminder>",
                ),
                test_text_block("tell me about simpsons"),
            ]),
        });

        let translated = translate_request(&req).expect("translate");
        let input = serialized_input(&translated);
        let metadata = translated.client_metadata.as_ref().expect("metadata");

        assert!(input.contains("tell me about simpsons"));
        assert_eq!(
            metadata
                .get("gateway_conversation_inclusion")
                .map(String::as_str),
            Some("read_only")
        );
        assert!(!metadata.contains_key("gateway_local_only_commands"));
    }

    #[test]
    fn literal_btw_in_normal_message_is_not_read_only() {
        let mut req = base_req();
        req.messages.push(AnthropicMessage {
            role: "user".to_string(),
            content: AnthropicContent::Text("next message after /btw".to_string()),
        });

        let translated = translate_request(&req).expect("translate");
        let metadata = translated.client_metadata.as_ref();

        assert!(
            metadata.is_none_or(|metadata| {
                !metadata.contains_key("gateway_conversation_inclusion")
            })
        );
    }

    #[test]
    fn active_slash_command_stays_active_after_tool_result_turns() {
        let mut req = base_req();
        req.messages.push(AnthropicMessage {
            role: "user".to_string(),
            content: AnthropicContent::Blocks(vec![
                AnthropicContentBlock {
                    block_type: "text".to_string(),
                    text: Some(
                        "<command-message>explain-feature</command-message>\n\
                         <command-name>/explain-feature</command-name>\n\
                         <command-args>explain the currently unstaged feature in this repo</command-args>"
                            .to_string(),
                    ),
                    id: None,
                    name: None,
                    input: None,
                    tool_use_id: None,
                    content: None,
                    is_error: None,
                    source: None,
                    extra: std::collections::BTreeMap::default(),
                },
                AnthropicContentBlock {
                    block_type: "text".to_string(),
                    text: Some(
                        "You are a code explainer for this repository.\n\
                         Use the requested structure."
                            .to_string(),
                    ),
                    id: None,
                    name: None,
                    input: None,
                    tool_use_id: None,
                    content: None,
                    is_error: None,
                    source: None,
                    extra: std::collections::BTreeMap::default(),
                },
            ]),
        });
        req.messages.push(AnthropicMessage {
            role: "assistant".to_string(),
            content: AnthropicContent::Blocks(vec![AnthropicContentBlock {
                block_type: "tool_use".to_string(),
                text: None,
                id: Some("call_task".to_string()),
                name: Some("TaskCreate".to_string()),
                input: Some(serde_json::json!({ "subject": "Inspect unstaged diff" })),
                tool_use_id: None,
                content: None,
                is_error: None,
                source: None,
                extra: std::collections::BTreeMap::default(),
            }]),
        });
        req.messages.push(AnthropicMessage {
            role: "user".to_string(),
            content: AnthropicContent::Blocks(vec![AnthropicContentBlock {
                block_type: "tool_result".to_string(),
                text: None,
                id: None,
                name: None,
                input: None,
                tool_use_id: Some("call_task".to_string()),
                content: Some(serde_json::json!("Task #1 created successfully")),
                is_error: None,
                source: None,
                extra: std::collections::BTreeMap::default(),
            }]),
        });

        let translated = translate_request(&req).expect("translate");
        let input = serialized_input(&translated);

        assert!(
            translated
                .instructions
                .contains("Use the requested structure.")
        );
        assert!(input.contains("explain the currently unstaged feature in this repo"));
        assert!(input.contains("\"type\":\"function_call\""));
        assert!(input.contains("\"type\":\"function_call_output\""));
    }

    #[test]
    fn active_claude_code_slash_command_can_be_disabled_by_config() {
        let mut req = base_req();
        req.messages.push(AnthropicMessage {
            role: "user".to_string(),
            content: AnthropicContent::Text(
                "<command-message>review_agent</command-message>\n\
                 <command-name>/review_agent</command-name>\n\
                 <command-args>verify implementation</command-args>\n\
                 Expanded command body"
                    .to_string(),
            ),
        });
        let mut claude_code_config = gateway_core::config::ClaudeCodeWorkflowConfig::default();
        claude_code_config.slash_commands.enabled = false;
        let tool_context =
            ToolTranslationContext::default().with_claude_code_config(claude_code_config);

        let translated = translate_request_with_context(&req, &tool_context).expect("translate");
        let input = serialized_input(&translated);

        assert!(!translated.instructions.contains("Expanded command body"));
        assert!(input.contains("/review_agent"));
        assert!(input.contains("Expanded command body"));
    }

    #[test]
    fn command_body_base_directory_line_is_rewritten_before_promotion() {
        let mut req = base_req();
        req.messages.push(AnthropicMessage {
            role: "user".to_string(),
            content: AnthropicContent::Blocks(vec![
                AnthropicContentBlock {
                    block_type: "text".to_string(),
                    text: Some(
                        "<command-message>example-skill</command-message>\n\
                         <command-name>/example-skill</command-name>\n\
                         <command-args>run the example workflow</command-args>"
                            .to_string(),
                    ),
                    id: None,
                    name: None,
                    input: None,
                    tool_use_id: None,
                    content: None,
                    is_error: None,
                    source: None,
                    extra: std::collections::BTreeMap::default(),
                },
                AnthropicContentBlock {
                    block_type: "text".to_string(),
                    text: Some(
                        "Base directory for this skill: test-fixtures/claude/skills/example-skill\n\n\
                         # Example Workflow\n\n\
                         Follow the example workflow."
                            .to_string(),
                    ),
                    id: None,
                    name: None,
                    input: None,
                    tool_use_id: None,
                    content: None,
                    is_error: None,
                    source: None,
                    extra: std::collections::BTreeMap::default(),
                },
            ]),
        });

        let translated = translate_request(&req).expect("translate");
        let input = serialized_input(&translated);

        assert!(
            translated
                .instructions
                .starts_with("Everything before the latest user message or slash command")
        );
        assert!(
            translated
                .instructions
                .contains(
                    "Base directory for this skill: test-fixtures/claude/skills/example-skill, analyze the files in this directory before proceeding"
                )
        );
        assert!(
            !translated
                .instructions
                .contains("The slash command instructions below are complete")
        );
        assert_eq!(
            translated
                .instructions
                .matches("Base directory for this skill:")
                .count(),
            1
        );
        assert!(translated.instructions.contains("# Example Workflow"));
        assert!(input.contains("run the example workflow"));
        assert_eq!(
            translated
                .client_metadata
                .as_ref()
                .and_then(|metadata| metadata.get("claude_code_slash_command"))
                .map(String::as_str),
            Some("/example-skill")
        );
    }

    #[test]
    fn translates_base64_image_to_data_url_input_image() {
        let mut req = base_req();
        req.messages.push(AnthropicMessage {
            role: "user".to_string(),
            content: AnthropicContent::Blocks(vec![AnthropicContentBlock {
                block_type: "image".to_string(),
                text: None,
                id: None,
                name: None,
                input: None,
                tool_use_id: None,
                content: None,
                is_error: None,
                source: Some(crate::types::AnthropicImageSource {
                    source_type: "base64".to_string(),
                    media_type: Some("image/png".to_string()),
                    data: Some("AAA=".to_string()),
                    extra: std::collections::BTreeMap::new(),
                }),
                extra: std::collections::BTreeMap::new(),
            }]),
        });

        let translated = translate_request(&req).expect("translate");
        let msg = translated
            .input
            .iter()
            .find(|v| v.get("type").and_then(|t| t.as_str()) == Some("message"))
            .expect("message item");
        let content0 = msg
            .get("content")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .expect("content[0]");
        assert_eq!(
            content0.get("type").and_then(|v| v.as_str()),
            Some("input_image")
        );
        assert_eq!(
            content0.get("image_url").and_then(|v| v.as_str()),
            Some("data:image/png;base64,AAA=")
        );
    }

    #[test]
    fn tool_result_output_is_wire_text_string() {
        let mut req = base_req();
        req.messages.push(AnthropicMessage {
            role: "user".to_string(),
            content: AnthropicContent::Blocks(vec![AnthropicContentBlock {
                block_type: "tool_result".to_string(),
                text: None,
                id: None,
                name: None,
                input: None,
                tool_use_id: Some("call_123".to_string()),
                content: Some(serde_json::json!([{ "type": "text", "text": "ok" }])),
                is_error: Some(false),
                source: None,
                extra: std::collections::BTreeMap::new(),
            }]),
        });

        let translated = translate_request(&req).expect("translate");
        let item = translated
            .input
            .iter()
            .find(|v| v.get("type").and_then(|t| t.as_str()) == Some("function_call_output"))
            .expect("function_call_output item");
        assert_eq!(
            item.get("call_id").and_then(|v| v.as_str()),
            Some("call_123")
        );
        let output = item.get("output").expect("output present");
        assert_eq!(
            output
                .as_array()
                .and_then(|arr| arr.first())
                .and_then(|v| v.get("type"))
                .and_then(|v| v.as_str()),
            Some("input_text")
        );
    }

    #[test]
    fn tool_definitions_fixture_translates_to_backend_tools() {
        let json = fixture("tools/tool_definitions.json");
        let req: AnthropicMessagesRequest = serde_json::from_str(&json).expect("parse request");
        let translated = translate_request(&req).expect("translate");

        assert_eq!(translated.tools.len(), 1);
        let tool = &translated.tools[0];
        assert_eq!(tool.get("type").and_then(|v| v.as_str()), Some("function"));
        assert_eq!(tool.get("name").and_then(|v| v.as_str()), Some("Read"));
        let params = tool.get("parameters").expect("parameters");
        assert_eq!(params.get("type").and_then(|v| v.as_str()), Some("object"));
        assert_eq!(
            params
                .get("additionalProperties")
                .and_then(serde_json::Value::as_bool),
            Some(false)
        );
    }

    #[test]
    fn agent_tool_schema_hides_isolation_from_backend() {
        let mut req = base_req();
        req.tools.push(AnthropicToolDefinition {
            name: "Agent".to_string(),
            tool_type: None,
            description: Some("Launch a subagent".to_string()),
            input_schema: serde_json::json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "description": { "type": "string" },
                    "prompt": { "type": "string" },
                    "isolation": { "type": "string", "enum": ["worktree"] }
                },
                "required": ["description", "prompt", "isolation"]
            }),
            allowed_domains: Vec::new(),
            blocked_domains: Vec::new(),
            max_uses: None,
            extra: std::collections::BTreeMap::new(),
        });

        let translated = translate_request(&req).expect("translate");
        let parameters = translated.tools[0].get("parameters").expect("parameters");
        let properties = parameters
            .get("properties")
            .and_then(serde_json::Value::as_object)
            .expect("properties object");
        let required = parameters
            .get("required")
            .and_then(serde_json::Value::as_array)
            .expect("required array");

        assert!(properties.get("isolation").is_none());
        assert!(
            !required
                .iter()
                .any(|field| field.as_str() == Some("isolation"))
        );
    }

    #[test]
    fn hosted_web_search_translates_to_openai_web_search() {
        let mut req = base_req();
        req.tools.push(AnthropicToolDefinition {
            name: "web_search".to_string(),
            tool_type: Some("web_search_20250305".to_string()),
            description: None,
            input_schema: serde_json::Value::Null,
            allowed_domains: vec!["github.com".to_string(), "docs.brew.sh".to_string()],
            blocked_domains: Vec::new(),
            max_uses: Some(8),
            extra: std::collections::BTreeMap::new(),
        });

        let translated = translate_request(&req).expect("translate");

        assert_eq!(translated.tools.len(), 1);
        assert_eq!(
            translated.tools[0].get("type").and_then(|v| v.as_str()),
            Some("web_search")
        );
        assert_eq!(
            translated.tools[0]
                .pointer("/filters/allowed_domains/0")
                .and_then(|v| v.as_str()),
            Some("github.com")
        );
        assert_eq!(
            translated.include,
            vec!["web_search_call.action.sources".to_string()]
        );
        assert_eq!(translated.tool_choice, "required");
    }

    #[test]
    fn hosted_web_search_rejects_blocked_domains() {
        let mut req = base_req();
        req.tools.push(AnthropicToolDefinition {
            name: "web_search".to_string(),
            tool_type: Some("web_search_20250305".to_string()),
            description: None,
            input_schema: serde_json::Value::Null,
            allowed_domains: Vec::new(),
            blocked_domains: vec!["example.com".to_string()],
            max_uses: Some(8),
            extra: std::collections::BTreeMap::new(),
        });

        let Err(error) = translate_request(&req) else {
            panic!("blocked_domains must fail");
        };
        assert!(error.contains("blocked_domains"));
        assert!(error.contains("unsupported"));
    }

    #[test]
    fn hosted_web_search_rejects_unknown_fields() {
        let mut req = base_req();
        req.tools.push(AnthropicToolDefinition {
            name: "web_search".to_string(),
            tool_type: Some("web_search_20250305".to_string()),
            description: None,
            input_schema: serde_json::Value::Null,
            allowed_domains: Vec::new(),
            blocked_domains: Vec::new(),
            max_uses: Some(8),
            extra: std::collections::BTreeMap::from_iter([(
                "mystery".to_string(),
                serde_json::Value::Bool(true),
            )]),
        });

        let Err(error) = translate_request(&req) else {
            panic!("unknown fields must fail");
        };
        assert!(error.contains("unsupported field"));
        assert!(error.contains("mystery"));
    }

    #[test]
    fn tool_result_rich_fixture_preserves_image_content_items() {
        let json = fixture("tools/tool_result_rich.json");
        let req: AnthropicMessagesRequest = serde_json::from_str(&json).expect("parse request");
        let translated = translate_request(&req).expect("translate");
        let item = translated
            .input
            .iter()
            .find(|v| v.get("type").and_then(|t| t.as_str()) == Some("function_call_output"))
            .expect("function_call_output item");

        let output = item.get("output").expect("output present");
        let arr = output.as_array().expect("output must be array");
        assert!(
            arr.iter()
                .any(|v| v.get("type").and_then(|t| t.as_str()) == Some("input_image")),
            "expected an input_image content item in tool_result output"
        );
    }

    #[test]
    fn tool_result_uses_custom_output_type_from_context() {
        let mut req = base_req();
        req.messages.push(AnthropicMessage {
            role: "user".to_string(),
            content: AnthropicContent::Blocks(vec![AnthropicContentBlock {
                block_type: "tool_result".to_string(),
                text: None,
                id: None,
                name: None,
                input: None,
                tool_use_id: Some("call_custom".to_string()),
                content: Some(serde_json::json!("ok")),
                is_error: Some(false),
                source: None,
                extra: std::collections::BTreeMap::new(),
            }]),
        });

        let translated = translate_request_with_context(
            &req,
            &context_with("call_custom", CodexToolCallKind::Custom),
        )
        .expect("translate");
        let item = translated.input.first().expect("output item");
        assert_eq!(
            item.get("type").and_then(|v| v.as_str()),
            Some("custom_tool_call_output")
        );
    }

    #[test]
    fn tool_result_uses_tool_search_output_type_from_context() {
        let mut req = base_req();
        req.messages.push(AnthropicMessage {
            role: "user".to_string(),
            content: AnthropicContent::Blocks(vec![AnthropicContentBlock {
                block_type: "tool_result".to_string(),
                text: None,
                id: None,
                name: None,
                input: None,
                tool_use_id: Some("call_search".to_string()),
                content: Some(
                    serde_json::json!({ "tools": [{ "type": "function", "name": "Read" }] }),
                ),
                is_error: Some(false),
                source: None,
                extra: std::collections::BTreeMap::new(),
            }]),
        });

        let translated = translate_request_with_context(
            &req,
            &context_with("call_search", CodexToolCallKind::ToolSearch),
        )
        .expect("translate");
        let item = translated.input.first().expect("output item");
        assert_eq!(
            item.get("type").and_then(|v| v.as_str()),
            Some("tool_search_output")
        );
        assert_eq!(
            item.get("tools").and_then(|v| v.as_array()).map(Vec::len),
            Some(1)
        );
    }

    #[test]
    fn tool_result_uses_function_output_for_local_shell_context() {
        let mut req = base_req();
        req.messages.push(AnthropicMessage {
            role: "user".to_string(),
            content: AnthropicContent::Blocks(vec![AnthropicContentBlock {
                block_type: "tool_result".to_string(),
                text: None,
                id: None,
                name: None,
                input: None,
                tool_use_id: Some("call_shell".to_string()),
                content: Some(serde_json::json!("ok")),
                is_error: Some(false),
                source: None,
                extra: std::collections::BTreeMap::new(),
            }]),
        });

        let translated = translate_request_with_context(
            &req,
            &context_with("call_shell", CodexToolCallKind::LocalShell),
        )
        .expect("translate");
        let item = translated.input.first().expect("output item");
        assert_eq!(
            item.get("type").and_then(|v| v.as_str()),
            Some("function_call_output")
        );
    }

    #[test]
    fn replayed_custom_tool_use_uses_custom_call_type_from_context() {
        let mut req = base_req();
        req.messages.push(AnthropicMessage {
            role: "assistant".to_string(),
            content: AnthropicContent::Blocks(vec![AnthropicContentBlock {
                block_type: "tool_use".to_string(),
                text: None,
                id: Some("call_custom".to_string()),
                name: Some("apply_patch".to_string()),
                input: Some(serde_json::json!({ "input": "*** Begin Patch\n*** End Patch\n" })),
                tool_use_id: None,
                content: None,
                is_error: None,
                source: None,
                extra: std::collections::BTreeMap::new(),
            }]),
        });

        let translated = translate_request_with_context(
            &req,
            &context_with("call_custom", CodexToolCallKind::Custom),
        )
        .expect("translate");
        let item = translated.input.first().expect("tool use item");
        assert_eq!(
            item.get("type").and_then(|v| v.as_str()),
            Some("custom_tool_call")
        );
        assert_eq!(
            item.get("input").and_then(|v| v.as_str()),
            Some("*** Begin Patch\n*** End Patch\n")
        );
    }
}
