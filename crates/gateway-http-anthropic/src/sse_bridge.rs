#![forbid(unsafe_code)]

use axum::response::sse::Event;
use gateway_state::ToolCallStore;
use std::collections::HashMap;

use crate::tool_arg_policy::{ToolArgContext, apply_policies};

#[derive(Debug, Clone, Copy)]
struct BackendTokenUsage {
    output_tokens: i64,
}

#[derive(Debug, Clone)]
struct BlockState {
    index: u32,
    closed: bool,
}

#[derive(Default)]
pub(crate) struct StreamState {
    // Anthropic stream indices
    next_block_index: u32,
    blocks: Vec<BlockState>,

    // Text routing
    active_text_index: u32,
    saw_output_text_delta: bool,

    // Thinking routing (optional)
    active_thinking_index: Option<u32>,

    // Tool routing
    tool_blocks_by_call_id: HashMap<String, u32>,
    tool_name_by_call_id: HashMap<String, String>,
    tool_args_buf_by_call_id: HashMap<String, String>,
    last_tool_call_id: Option<String>,

    // Backend usage snapshot (emitted on `response.completed`).
    completed_usage: Option<BackendTokenUsage>,

    pub(crate) completed: bool,
}

impl StreamState {
    pub(crate) fn new_with_text_block0_started() -> Self {
        Self {
            next_block_index: 1,
            blocks: vec![BlockState {
                index: 0,
                closed: false,
            }],
            active_text_index: 0,
            saw_output_text_delta: false,
            active_thinking_index: None,
            tool_blocks_by_call_id: HashMap::new(),
            tool_name_by_call_id: HashMap::new(),
            tool_args_buf_by_call_id: HashMap::new(),
            last_tool_call_id: None,
            completed_usage: None,
            completed: false,
        }
    }

    fn add_block(&mut self) -> u32 {
        let index = self.next_block_index;
        self.next_block_index = self.next_block_index.saturating_add(1);
        self.blocks.push(BlockState {
            index,
            closed: false,
        });
        index
    }

    fn open_thinking_block_if_needed(&mut self) -> (u32, bool) {
        if let Some(idx) = self.active_thinking_index {
            return (idx, false);
        }
        let idx = self.add_block();
        self.active_thinking_index = Some(idx);
        (idx, true)
    }

    fn ensure_tool_block(&mut self, call_id: &str) -> (u32, bool) {
        if let Some(idx) = self.tool_blocks_by_call_id.get(call_id).copied() {
            return (idx, false);
        }
        let idx = self.add_block();
        self.tool_blocks_by_call_id.insert(call_id.to_string(), idx);
        self.tool_args_buf_by_call_id
            .entry(call_id.to_string())
            .or_default();
        (idx, true)
    }
}

// Note: the initial text block (index 0) is started by `anthropic_stream_start_events`.

fn content_block_start_tool_use(index: u32, call_id: &str, name: &str) -> Event {
    let payload = serde_json::json!({
        "type": "content_block_start",
        "index": index,
        "content_block": {
            "type": "tool_use",
            "id": call_id,
            "name": name,
            "input": {}
        }
    })
    .to_string();
    Event::default().event("content_block_start").data(payload)
}

fn content_block_start_thinking(index: u32) -> Event {
    // Anthropic thinking blocks typically include `thinking` and may include `signature`.
    let payload = serde_json::json!({
        "type": "content_block_start",
        "index": index,
        "content_block": { "type": "thinking", "thinking": "", "signature": "" }
    })
    .to_string();
    Event::default().event("content_block_start").data(payload)
}

fn content_block_delta_text(index: u32, text: &str) -> Event {
    let payload = serde_json::json!({
        "type": "content_block_delta",
        "index": index,
        "delta": { "type": "text_delta", "text": text }
    })
    .to_string();
    Event::default().event("content_block_delta").data(payload)
}

fn content_block_delta_input_json(index: u32, delta: &str) -> Event {
    let payload = serde_json::json!({
        "type": "content_block_delta",
        "index": index,
        "delta": { "type": "input_json_delta", "partial_json": delta }
    })
    .to_string();
    Event::default().event("content_block_delta").data(payload)
}

fn content_block_delta_thinking(index: u32, delta: &str) -> Event {
    let payload = serde_json::json!({
        "type": "content_block_delta",
        "index": index,
        "delta": { "type": "thinking_delta", "thinking": delta }
    })
    .to_string();
    Event::default().event("content_block_delta").data(payload)
}

fn content_block_stop(index: u32) -> Event {
    Event::default()
        .event("content_block_stop")
        .data(serde_json::json!({"type":"content_block_stop","index":index}).to_string())
}

fn message_delta(stop_reason: &str, usage: Option<BackendTokenUsage>) -> Event {
    let output_tokens = usage.map_or(0, |u| u.output_tokens);
    Event::default().event("message_delta").data(
        serde_json::json!({
            "type":"message_delta",
            "delta":{"stop_reason":stop_reason,"stop_sequence":null},
            "usage":{"output_tokens":output_tokens}
        })
        .to_string(),
    )
}

fn message_stop() -> Event {
    Event::default()
        .event("message_stop")
        .data(serde_json::json!({"type":"message_stop"}).to_string())
}

fn error_event(message: &str) -> Vec<Event> {
    let payload = serde_json::json!({
        "type": "error",
        "error": { "type": "backend_error", "message": message }
    })
    .to_string();
    vec![Event::default().event("error").data(payload)]
}

fn validate_tool_args_json_object(buf: &str) -> Result<(), String> {
    let trimmed = buf.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    let value: serde_json::Value = serde_json::from_str(trimmed)
        .map_err(|e| format!("tool_use.input is not valid JSON: {e}"))?;
    if !value.is_object() {
        return Err("tool_use.input must be a JSON object".to_string());
    }
    Ok(())
}

fn parse_tool_args_object(buf: &str) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let trimmed = buf.trim();
    if trimmed.is_empty() {
        return Ok(serde_json::Map::new());
    }
    let value: serde_json::Value = serde_json::from_str(trimmed)
        .map_err(|e| format!("tool_use.input is not valid JSON: {e}"))?;
    let obj = value
        .as_object()
        .cloned()
        .ok_or_else(|| "tool_use.input must be a JSON object".to_string())?;
    Ok(obj)
}

fn tool_args_delta_from_object(
    index: u32,
    obj: &serde_json::Map<String, serde_json::Value>,
) -> Event {
    let json = serde_json::to_string(&serde_json::Value::Object(obj.clone()))
        .unwrap_or_else(|_| "{}".to_string());
    content_block_delta_input_json(index, &json)
}

pub(crate) fn finalize_message(st: &mut StreamState) -> Vec<Event> {
    st.completed = true;

    let mut out = Vec::new();
    for block in &mut st.blocks {
        if block.closed {
            continue;
        }
        out.push(content_block_stop(block.index));
        block.closed = true;
    }

    let stop_reason = if st.tool_blocks_by_call_id.is_empty() {
        "end_turn"
    } else {
        "tool_use"
    };
    out.push(message_delta(stop_reason, st.completed_usage));
    out.push(message_stop());
    out
}

fn parse_delta_event_fields(data: &str) -> Option<(Option<String>, Option<String>, String)> {
    let v = serde_json::from_str::<serde_json::Value>(data).ok()?;
    let delta = v.get("delta").and_then(|v| v.as_str())?.to_string();
    let call_id = v
        .get("call_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let item_id = v
        .get("item_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    Some((call_id, item_id, delta))
}

pub(crate) fn map_backend_event(
    st: &mut StreamState,
    event_name: &str,
    data: &str,
    extract_stream_delta_text: impl Fn(&str) -> Option<String>,
    tool_calls: &ToolCallStore,
    request_id: Option<&str>,
) -> Option<Vec<Event>> {
    match event_name {
        "response.output_text.delta" => {
            handle_output_text_delta(st, data, extract_stream_delta_text)
        }
        "response.output_item.added" | "response.output_item.done" => {
            handle_output_item(st, data, tool_calls, request_id)
        }
        "response.function_call_arguments.delta" | "response.custom_tool_call_input.delta" => {
            handle_tool_arg_delta(st, data)
        }
        "response.reasoning_text.delta" | "response.reasoning_summary_text.delta" => {
            handle_reasoning_delta(st, data)
        }
        "response.completed" => Some(handle_completed(st, data, request_id)),
        _ => None,
    }
}

fn handle_output_text_delta(
    st: &mut StreamState,
    data: &str,
    extract_stream_delta_text: impl Fn(&str) -> Option<String>,
) -> Option<Vec<Event>> {
    let text = extract_stream_delta_text(data)?;
    st.saw_output_text_delta = true;
    Some(vec![content_block_delta_text(st.active_text_index, &text)])
}

fn handle_output_item(
    st: &mut StreamState,
    data: &str,
    tool_calls: &ToolCallStore,
    request_id: Option<&str>,
) -> Option<Vec<Event>> {
    let v = serde_json::from_str::<serde_json::Value>(data).ok()?;
    let item = v.get("item")?;
    match item.get("type").and_then(|v| v.as_str()) {
        Some("function_call") => handle_function_call_item(st, item, tool_calls, request_id),
        Some("message") => handle_message_item(st, item),
        _ => None,
    }
}

fn handle_function_call_item(
    st: &mut StreamState,
    item: &serde_json::Value,
    tool_calls: &ToolCallStore,
    request_id: Option<&str>,
) -> Option<Vec<Event>> {
    let call_id = item.get("call_id").and_then(|s| s.as_str())?;
    let name = item.get("name").and_then(|s| s.as_str()).unwrap_or("");

    let (tool_index, is_new) = st.ensure_tool_block(call_id);
    st.last_tool_call_id = Some(call_id.to_string());
    st.tool_name_by_call_id
        .insert(call_id.to_string(), name.to_string());

    if is_new {
        let _ = tool_calls.record_tool_call(call_id, name, request_id);
        Some(vec![content_block_start_tool_use(
            tool_index, call_id, name,
        )])
    } else {
        None
    }
}

fn handle_message_item(st: &mut StreamState, item: &serde_json::Value) -> Option<Vec<Event>> {
    if st.saw_output_text_delta {
        return None;
    }
    let content = item.get("content").and_then(|v| v.as_array())?;
    let mut out = Vec::new();
    for c in content {
        if c.get("type").and_then(|v| v.as_str()) != Some("output_text") {
            continue;
        }
        let Some(text) = c.get("text").and_then(|v| v.as_str()) else {
            continue;
        };
        if text.is_empty() {
            continue;
        }
        out.push(content_block_delta_text(st.active_text_index, text));
    }
    (!out.is_empty()).then_some(out)
}

fn handle_tool_arg_delta(st: &mut StreamState, data: &str) -> Option<Vec<Event>> {
    let (call_id, _item_id, delta) = parse_delta_event_fields(data)?;
    let call_id = call_id.or_else(|| st.last_tool_call_id.clone())?;

    let (tool_index, is_new) = st.ensure_tool_block(&call_id);
    let buf = st
        .tool_args_buf_by_call_id
        .entry(call_id.clone())
        .or_default();
    buf.push_str(&delta);

    let mut out = Vec::new();
    if is_new {
        out.push(content_block_start_tool_use(tool_index, &call_id, ""));
    }
    (!out.is_empty()).then_some(out)
}

fn handle_reasoning_delta(st: &mut StreamState, data: &str) -> Option<Vec<Event>> {
    let (_call_id, _item_id, delta) = parse_delta_event_fields(data)?;
    let (idx, started) = st.open_thinking_block_if_needed();
    let mut out = Vec::new();
    if started {
        out.push(content_block_start_thinking(idx));
    }
    out.push(content_block_delta_thinking(idx, &delta));
    Some(out)
}

#[derive(serde::Deserialize)]
struct BackendCompletedEnvelope {
    response: Option<BackendCompletedResponse>,
}

#[derive(serde::Deserialize)]
struct BackendCompletedResponse {
    usage: Option<BackendCompletedUsage>,
}

#[derive(serde::Deserialize)]
struct BackendCompletedUsage {
    input_tokens: i64,
    output_tokens: i64,
}

fn parse_completed_usage(data: &str) -> Option<BackendTokenUsage> {
    let value = serde_json::from_str::<serde_json::Value>(data).ok()?;
    let env: BackendCompletedEnvelope = serde_json::from_value(value).ok()?;
    let usage = env.response?.usage?;
    let _ = usage.input_tokens;
    Some(BackendTokenUsage {
        output_tokens: usage.output_tokens,
    })
}

fn handle_completed(st: &mut StreamState, data: &str, request_id: Option<&str>) -> Vec<Event> {
    st.completed_usage = parse_completed_usage(data);

    for buf in st.tool_args_buf_by_call_id.values() {
        if let Err(err) = validate_tool_args_json_object(buf) {
            return error_event(&err);
        }
    }

    let mut tool_calls_by_index: Vec<(&String, u32)> = st
        .tool_blocks_by_call_id
        .iter()
        .map(|(call_id, idx)| (call_id, *idx))
        .collect();
    tool_calls_by_index.sort_by_key(|(_, idx)| *idx);

    let mut out = Vec::new();
    for (call_id, tool_index) in tool_calls_by_index {
        let buf = st
            .tool_args_buf_by_call_id
            .get(call_id)
            .map_or("", String::as_str);
        let mut obj = match parse_tool_args_object(buf) {
            Ok(v) => v,
            Err(err) => return error_event(&err),
        };

        let tool_name = st
            .tool_name_by_call_id
            .get(call_id)
            .map_or("", String::as_str);
        let ctx = ToolArgContext { tool_name };
        let edits = apply_policies(&ctx, &mut obj);
        if !edits.is_empty() {
            tracing::info!(
                request_id,
                call_id,
                tool_name,
                edits = ?edits,
                "sanitized tool arguments"
            );
        }

        out.push(tool_args_delta_from_object(tool_index, &obj));
    }

    out.extend(finalize_message(st));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_args_validation_requires_object() {
        let err = validate_tool_args_json_object("[]").expect_err("should reject non-object");
        assert_eq!(err, "tool_use.input must be a JSON object");
    }

    #[test]
    fn tool_args_validation_accepts_empty() {
        validate_tool_args_json_object("").expect("empty ok");
    }

    #[test]
    fn parse_tool_args_object_accepts_empty_as_object() {
        let obj = parse_tool_args_object("").expect("empty ok");
        assert!(obj.is_empty());
    }

    #[test]
    fn parse_completed_usage_extracts_tokens() {
        let json = r#"{"type":"response.completed","response":{"id":"r1","usage":{"input_tokens":7,"output_tokens":9}}}"#;
        let got = parse_completed_usage(json).expect("usage");
        assert_eq!(got.output_tokens, 9);
    }
}
