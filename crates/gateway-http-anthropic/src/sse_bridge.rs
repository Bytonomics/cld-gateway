#![forbid(unsafe_code)]

use axum::response::sse::Event;
use gateway_state::ToolCallStore;
use std::collections::HashMap;

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
    tool_args_buf_by_call_id: HashMap<String, String>,
    last_tool_call_id: Option<String>,

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
            tool_args_buf_by_call_id: HashMap::new(),
            last_tool_call_id: None,
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

fn message_delta(stop_reason: &str) -> Event {
    Event::default().event("message_delta").data(
        serde_json::json!({
            "type":"message_delta",
            "delta":{"stop_reason":stop_reason,"stop_sequence":null},
            "usage":{"output_tokens":0}
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
    out.push(message_delta(stop_reason));
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
            let text = extract_stream_delta_text(data)?;
            st.saw_output_text_delta = true;
            Some(vec![content_block_delta_text(st.active_text_index, &text)])
        }
        "response.output_item.added" | "response.output_item.done" => {
            let v = serde_json::from_str::<serde_json::Value>(data).ok()?;
            let item = v.get("item")?;
            match item.get("type").and_then(|v| v.as_str()) {
                Some("function_call") => {
                    let call_id = item.get("call_id").and_then(|s| s.as_str())?;
                    let name = item.get("name").and_then(|s| s.as_str()).unwrap_or("");

                    let (tool_index, is_new) = st.ensure_tool_block(call_id);
                    st.last_tool_call_id = Some(call_id.to_string());

                    if is_new {
                        let _ = tool_calls.record_tool_call(call_id, name, request_id);
                        return Some(vec![content_block_start_tool_use(
                            tool_index, call_id, name,
                        )]);
                    }
                    None
                }
                Some("message") => {
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
                _ => None,
            }
        }
        "response.function_call_arguments.delta" | "response.custom_tool_call_input.delta" => {
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
                // We did not receive output_item.* yet; emit a minimal tool_use start.
                out.push(content_block_start_tool_use(tool_index, &call_id, ""));
            }
            out.push(content_block_delta_input_json(tool_index, &delta));
            Some(out)
        }
        "response.reasoning_text.delta" | "response.reasoning_summary_text.delta" => {
            let (_call_id, _item_id, delta) = parse_delta_event_fields(data)?;
            let (idx, started) = st.open_thinking_block_if_needed();
            let mut out = Vec::new();
            if started {
                out.push(content_block_start_thinking(idx));
            }
            out.push(content_block_delta_thinking(idx, &delta));
            Some(out)
        }
        "response.completed" => {
            // Validate buffered tool args for all tool blocks.
            for buf in st.tool_args_buf_by_call_id.values() {
                if let Err(err) = validate_tool_args_json_object(buf) {
                    return Some(error_event(&err));
                }
            }
            Some(finalize_message(st))
        }
        _ => None,
    }
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
}
