#![forbid(unsafe_code)]

use axum::response::sse::Event;
use gateway_backend_codex::sse_unary::extract_usage_from_completed_event;
use gateway_backend_codex::sse_unary::record_completed_web_search_call_ids;
use gateway_backend_codex::tool_calls::parse_output_item_tool_call;
use gateway_backend_codex::types::{CodexTokenUsage, CodexToolCall, CodexToolCallKind};
use gateway_state::ToolCallStore;
use std::collections::BTreeSet;
use std::collections::HashMap;

use crate::tool_arg_policy::sanitized_tool_args_for_kind;

#[derive(Debug, Clone)]
struct BlockState {
    index: u32,
    closed: bool,
}

#[derive(Debug, Clone)]
struct WebSearchCall {
    call_id: String,
    server_tool_use_id: String,
    query: Option<String>,
    results: Vec<serde_json::Value>,
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
    tool_kind_by_call_id: HashMap<String, CodexToolCallKind>,
    tool_args_buf_by_call_id: HashMap<String, String>,
    last_tool_call_id: Option<String>,

    // Backend usage snapshot (emitted on `response.completed`).
    completed_usage: Option<CodexTokenUsage>,
    context_management: Option<serde_json::Value>,
    completed_web_search_call_ids: BTreeSet<String>,
    emitted_web_search_call_ids: BTreeSet<String>,

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
            tool_kind_by_call_id: HashMap::new(),
            tool_args_buf_by_call_id: HashMap::new(),
            last_tool_call_id: None,
            completed_usage: None,
            context_management: None,
            completed_web_search_call_ids: BTreeSet::new(),
            emitted_web_search_call_ids: BTreeSet::new(),
            completed: false,
        }
    }

    pub(crate) fn with_context_management(
        mut self,
        context_management: Option<serde_json::Value>,
    ) -> Self {
        self.context_management = context_management;
        self
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

fn content_block_start_server_tool_use(
    index: u32,
    server_tool_use_id: &str,
    query: Option<&str>,
) -> Event {
    let input = query.map_or_else(
        || serde_json::json!({}),
        |query_text| serde_json::json!({ "query": query_text }),
    );
    let payload = serde_json::json!({
        "type": "content_block_start",
        "index": index,
        "content_block": {
            "type": "server_tool_use",
            "id": server_tool_use_id,
            "name": "web_search",
            "input": input
        }
    })
    .to_string();
    Event::default().event("content_block_start").data(payload)
}

fn content_block_start_web_search_tool_result(
    index: u32,
    server_tool_use_id: &str,
    results: &[serde_json::Value],
) -> Event {
    let payload = serde_json::json!({
        "type": "content_block_start",
        "index": index,
        "content_block": {
            "type": "web_search_tool_result",
            "tool_use_id": server_tool_use_id,
            "content": results
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

fn message_delta(
    stop_reason: &str,
    usage: Option<CodexTokenUsage>,
    context_management: Option<serde_json::Value>,
) -> Event {
    let mut payload = serde_json::json!({
            "type":"message_delta",
            "delta":{"stop_reason":stop_reason,"stop_sequence":null}
    });
    if let Some(token_usage) = usage {
        payload["usage"] = anthropic_usage_value(token_usage);
    }
    if let Some(context_management) = context_management {
        payload["context_management"] = context_management;
    }
    Event::default()
        .event("message_delta")
        .data(payload.to_string())
}

fn anthropic_usage_value(usage: CodexTokenUsage) -> serde_json::Value {
    let uncached_input_tokens = usage.input_tokens.saturating_sub(usage.cached_input_tokens);
    let mut value = serde_json::json!({
        "input_tokens": uncached_input_tokens,
        "cache_creation_input_tokens": 0,
        "cache_read_input_tokens": usage.cached_input_tokens,
        "output_tokens": usage.output_tokens,
    });
    if usage.web_search_requests > 0 {
        value["server_tool_use"] = serde_json::json!({
            "web_search_requests": usage.web_search_requests,
            "web_fetch_requests": 0
        });
    }
    value
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
    out.push(message_delta(
        stop_reason,
        st.completed_usage,
        st.context_management.clone(),
    ));
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
    if let Some(message) =
        gateway_backend_codex::backend_error::parse_backend_failure_event(event_name, data)
    {
        st.completed = true;
        return Some(error_event(&format!("backend stream failed: {message}")));
    }
    record_completed_web_search_call_ids(&mut st.completed_web_search_call_ids, event_name, data);

    match event_name {
        "response.output_text.delta" => {
            handle_output_text_delta(st, data, extract_stream_delta_text)
        }
        "response.output_item.added" | "response.output_item.done" => {
            handle_output_item(st, event_name, data, tool_calls, request_id)
        }
        "response.web_search_call.completed" => handle_web_search_call_event(st, event_name, data),
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
    event_name: &str,
    data: &str,
    tool_calls: &ToolCallStore,
    request_id: Option<&str>,
) -> Option<Vec<Event>> {
    if let Some(tool_call) = parse_output_item_tool_call(event_name, data) {
        return handle_tool_call_item(
            st,
            tool_call,
            event_name == "response.output_item.done",
            tool_calls,
            request_id,
        );
    }

    let v = serde_json::from_str::<serde_json::Value>(data).ok()?;
    let item = v.get("item")?;
    match item.get("type").and_then(|v| v.as_str()) {
        Some("web_search_call") => handle_web_search_call_item(st, item),
        Some("message") => handle_message_item(st, item),
        _ => None,
    }
}

fn handle_tool_call_item(
    st: &mut StreamState,
    tool_call: CodexToolCall,
    final_item: bool,
    tool_calls: &ToolCallStore,
    request_id: Option<&str>,
) -> Option<Vec<Event>> {
    let call_id = tool_call.call_id;
    let name = tool_call.name;
    let kind = tool_call.kind;

    let (tool_index, is_new) = st.ensure_tool_block(&call_id);
    st.last_tool_call_id = Some(call_id.clone());
    st.tool_name_by_call_id
        .insert(call_id.clone(), name.clone());
    st.tool_kind_by_call_id.insert(call_id.clone(), kind);
    if final_item {
        st.tool_args_buf_by_call_id
            .insert(call_id.clone(), tool_call.arguments);
    }

    if is_new {
        let _ = tool_calls.record_tool_call(&call_id, &name, kind.as_str(), request_id);
        Some(vec![content_block_start_tool_use(
            tool_index, &call_id, &name,
        )])
    } else {
        None
    }
}

fn handle_message_item(st: &mut StreamState, item: &serde_json::Value) -> Option<Vec<Event>> {
    if st.saw_output_text_delta {
        return None;
    }
    let out: Vec<Event> = gateway_backend_codex::output_text::message_item_output_texts(item)
        .iter()
        .map(|text| content_block_delta_text(st.active_text_index, text))
        .collect();
    (!out.is_empty()).then_some(out)
}

fn handle_web_search_call_event(
    st: &mut StreamState,
    event_name: &str,
    data: &str,
) -> Option<Vec<Event>> {
    let event = serde_json::from_str::<serde_json::Value>(data).ok()?;
    let call = web_search_call_from_value(event_name, &event)?;
    emit_web_search_call_blocks(st, call)
}

fn handle_web_search_call_item(
    st: &mut StreamState,
    item: &serde_json::Value,
) -> Option<Vec<Event>> {
    let call = web_search_call_from_item(item, "output_item")?;
    emit_web_search_call_blocks(st, call)
}

fn emit_web_search_call_blocks(st: &mut StreamState, call: WebSearchCall) -> Option<Vec<Event>> {
    if !st.emitted_web_search_call_ids.insert(call.call_id) {
        return None;
    }

    let server_tool_index = st.add_block();
    let result_index = st.add_block();
    Some(vec![
        content_block_start_server_tool_use(
            server_tool_index,
            &call.server_tool_use_id,
            call.query.as_deref(),
        ),
        content_block_stop(server_tool_index),
        content_block_start_web_search_tool_result(
            result_index,
            &call.server_tool_use_id,
            &call.results,
        ),
        content_block_stop(result_index),
    ])
}

fn web_search_call_from_value(
    event_name: &str,
    event: &serde_json::Value,
) -> Option<WebSearchCall> {
    if event.get("type").and_then(serde_json::Value::as_str) != Some(event_name) {
        return None;
    }
    let call_id = event
        .get("id")
        .or_else(|| event.get("call_id"))
        .or_else(|| event.get("item_id"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            event
                .get("output_index")
                .and_then(serde_json::Value::as_u64)
                .map(|index| format!("{event_name}:{index}"))
        })
        .unwrap_or_else(|| event_name.to_string());
    Some(WebSearchCall {
        server_tool_use_id: server_tool_use_id(&call_id),
        query: web_search_query(event),
        results: web_search_results(event),
        call_id,
    })
}

fn web_search_call_from_item(item: &serde_json::Value, fallback: &str) -> Option<WebSearchCall> {
    if item.get("type").and_then(serde_json::Value::as_str) != Some("web_search_call") {
        return None;
    }
    if item
        .get("status")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|status| status != "completed")
    {
        return None;
    }

    let call_id = item
        .get("id")
        .or_else(|| item.get("call_id"))
        .or_else(|| item.get("item_id"))
        .and_then(serde_json::Value::as_str)
        .map_or_else(|| fallback.to_string(), str::to_string);
    Some(WebSearchCall {
        server_tool_use_id: server_tool_use_id(&call_id),
        query: web_search_query(item),
        results: web_search_results(item),
        call_id,
    })
}

fn server_tool_use_id(call_id: &str) -> String {
    let safe_suffix: String = call_id
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || *character == '_')
        .collect();
    if safe_suffix.is_empty() {
        "srvtoolu_web_search".to_string()
    } else if safe_suffix.starts_with("srvtoolu_") {
        safe_suffix
    } else {
        format!("srvtoolu_{safe_suffix}")
    }
}

fn web_search_query(value: &serde_json::Value) -> Option<String> {
    value
        .pointer("/action/query")
        .or_else(|| value.pointer("/input/query"))
        .or_else(|| value.get("query"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

fn web_search_results(value: &serde_json::Value) -> Vec<serde_json::Value> {
    value
        .pointer("/action/sources")
        .or_else(|| value.get("sources"))
        .and_then(serde_json::Value::as_array)
        .map_or_else(Vec::new, |sources| {
            sources
                .iter()
                .filter_map(web_search_result_from_source)
                .collect()
        })
}

fn web_search_result_from_source(source: &serde_json::Value) -> Option<serde_json::Value> {
    let url = source.get("url").and_then(serde_json::Value::as_str)?;
    let mut result = serde_json::json!({
        "type": "web_search_result",
        "url": url,
    });
    if let Some(title) = source.get("title").and_then(serde_json::Value::as_str) {
        result["title"] = serde_json::Value::String(title.to_string());
    }
    if let Some(page_age) = source.get("page_age").and_then(serde_json::Value::as_str) {
        result["page_age"] = serde_json::Value::String(page_age.to_string());
    }
    if let Some(encrypted_content) = source
        .get("encrypted_content")
        .and_then(serde_json::Value::as_str)
    {
        result["encrypted_content"] = serde_json::Value::String(encrypted_content.to_string());
    }
    Some(result)
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

fn handle_completed(st: &mut StreamState, data: &str, request_id: Option<&str>) -> Vec<Event> {
    st.completed_usage = extract_usage_from_completed_event(data);
    if let Some(usage) = st.completed_usage.as_mut() {
        usage.web_search_requests =
            u32::try_from(st.completed_web_search_call_ids.len()).unwrap_or(u32::MAX);
    }

    let mut out = web_search_calls_from_completed(data)
        .into_iter()
        .filter_map(|call| emit_web_search_call_blocks(st, call))
        .flatten()
        .collect::<Vec<_>>();

    let mut tool_calls_by_index: Vec<(&String, u32)> = st
        .tool_blocks_by_call_id
        .iter()
        .map(|(call_id, idx)| (call_id, *idx))
        .collect();
    tool_calls_by_index.sort_by_key(|(_, idx)| *idx);

    for (call_id, tool_index) in tool_calls_by_index {
        let buf = st
            .tool_args_buf_by_call_id
            .get(call_id)
            .map_or("", String::as_str);
        let kind = st
            .tool_kind_by_call_id
            .get(call_id)
            .copied()
            .unwrap_or(CodexToolCallKind::Function);
        let tool_name = st
            .tool_name_by_call_id
            .get(call_id)
            .map_or("", String::as_str);
        let (obj, edits) = match sanitized_tool_args_for_kind(tool_name, kind, buf) {
            Ok(value) => value,
            Err(err) => return error_event(&err),
        };
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

fn web_search_calls_from_completed(data: &str) -> Vec<WebSearchCall> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
        return Vec::new();
    };
    value
        .pointer("/response/output")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(index, item)| {
            web_search_call_from_item(item, &format!("response_output_{index}"))
        })
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use axum::response::IntoResponse as _;
    use axum::response::sse::Sse;
    use bytes::Bytes;
    use eventsource_stream::Eventsource as _;
    use futures_util::StreamExt as _;
    use futures_util::stream;
    use gateway_core::DEFAULT_BACKEND_MODEL;
    use std::convert::Infallible;
    use uuid::Uuid;

    #[test]
    fn message_delta_includes_cumulative_usage() {
        let payload = anthropic_usage_value(CodexTokenUsage {
            input_tokens: 7,
            cached_input_tokens: 3,
            output_tokens: 9,
            reasoning_output_tokens: 2,
            total_tokens: 16,
            web_search_requests: 2,
        });
        assert_eq!(payload["input_tokens"], 4);
        assert_eq!(payload["cache_creation_input_tokens"], 0);
        assert_eq!(payload["cache_read_input_tokens"], 3);
        assert_eq!(payload["output_tokens"], 9);
        assert_eq!(payload["server_tool_use"]["web_search_requests"], 2);
        assert_eq!(payload["server_tool_use"]["web_fetch_requests"], 0);
    }

    fn fixture(path: &str) -> String {
        let full = format!("{}/tests/fixtures/{}", env!("CARGO_MANIFEST_DIR"), path);
        std::fs::read_to_string(full)
            .expect("read fixture")
            .replace("__DEFAULT_BACKEND_MODEL__", DEFAULT_BACKEND_MODEL)
    }

    fn parse_expected_jsonl(path: &str) -> Vec<(String, serde_json::Value)> {
        let text = fixture(path);
        text.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|line| {
                let v: serde_json::Value = serde_json::from_str(line).expect("jsonl line");
                let event = v
                    .get("event")
                    .and_then(|v| v.as_str())
                    .expect("event")
                    .to_string();
                let data = v.get("data").cloned().expect("data");
                (event, data)
            })
            .collect()
    }

    fn parse_sse_frames(body: &str) -> Vec<(String, serde_json::Value)> {
        body.split("\n\n")
            .filter_map(|frame| {
                let frame = frame.trim();
                if frame.is_empty() {
                    return None;
                }
                let mut event: Option<String> = None;
                let mut data_lines: Vec<&str> = Vec::new();
                for line in frame.lines() {
                    if let Some(rest) = line.strip_prefix("event:") {
                        event = Some(rest.trim().to_string());
                    } else if let Some(rest) = line.strip_prefix("data:") {
                        data_lines.push(rest.trim());
                    }
                }
                let event = event?;
                let data_str = data_lines.join("\n");
                let data: serde_json::Value =
                    serde_json::from_str(&data_str).expect("event data json");
                Some((event, data))
            })
            .collect()
    }

    fn normalize_msg_id(
        mut events: Vec<(String, serde_json::Value)>,
    ) -> Vec<(String, serde_json::Value)> {
        for (ev, data) in &mut events {
            if ev == "message_start"
                && let Some(msg) = data.get_mut("message")
                && let Some(obj) = msg.as_object_mut()
            {
                obj.insert(
                    "id".to_string(),
                    serde_json::Value::String("msg_TEST".to_string()),
                );
            }
        }
        events
    }

    fn extract_delta_text_simple(data: &str) -> Option<String> {
        let v = serde_json::from_str::<serde_json::Value>(data).ok()?;
        v.get("delta").and_then(|v| v.as_str()).map(str::to_string)
    }

    fn start_events(msg_id: &str, model: &str) -> Vec<Event> {
        vec![
            Event::default().event("message_start").data(
                serde_json::json!({
                    "type": "message_start",
                    "message": {
                        "id": msg_id,
                        "type": "message",
                        "role": "assistant",
                        "content": [],
                        "model": model,
                        "stop_reason": null,
                        "stop_sequence": null,
                        "usage": {
                            "input_tokens": 0,
                            "cache_creation_input_tokens": 0,
                            "cache_read_input_tokens": 0,
                            "output_tokens": 0
                        }
                    }
                })
                .to_string(),
            ),
            Event::default().event("content_block_start").data(
                serde_json::json!({
                    "type": "content_block_start",
                    "index": 0,
                    "content_block": { "type": "text", "text": "" }
                })
                .to_string(),
            ),
        ]
    }

    async fn run_bridge_and_capture(
        backend_sse_fixture: &str,
        model: &str,
        request_id: Option<&str>,
    ) -> Vec<(String, serde_json::Value)> {
        let backend_sse_fixture = format!("{backend_sse_fixture}\n\n");
        let byte_stream = stream::iter(vec![Ok::<Bytes, std::io::Error>(Bytes::from(
            backend_sse_fixture,
        ))]);
        let mut backend = Box::pin(byte_stream.eventsource());

        let mut state = StreamState::new_with_text_block0_started();
        let tool_calls_path =
            std::env::temp_dir().join(format!("gateway_tool_calls_{}.sqlite", Uuid::new_v4()));
        let tool_calls = ToolCallStore::new(&tool_calls_path);

        let mut events = start_events("msg_TEST", model);

        while let Some(next) = backend.next().await {
            let evt = next.expect("sse parse ok");
            let data = evt.data.trim();
            if data.is_empty() || data == "[DONE]" {
                continue;
            }
            if let Some(mut mapped) = map_backend_event(
                &mut state,
                evt.event.as_str(),
                data,
                extract_delta_text_simple,
                &tool_calls,
                request_id,
            ) {
                events.append(&mut mapped);
            }
        }

        let sse = Sse::new(stream::iter(
            events.into_iter().map(Ok::<Event, Infallible>),
        ));
        let res = sse.into_response();
        let body = to_bytes(res.into_body(), usize::MAX)
            .await
            .expect("read body");
        let text = std::str::from_utf8(&body).expect("utf8");
        normalize_msg_id(parse_sse_frames(text))
    }

    #[tokio::test]
    async fn streaming_bridge_matches_text_only_fixture() {
        let backend = fixture("streaming/backend_stream_text_only.sse");
        let got = run_bridge_and_capture(&backend, DEFAULT_BACKEND_MODEL, None).await;
        let expected = parse_expected_jsonl("streaming/expected_anthropic_text_only.jsonl");
        assert_eq!(got, expected);
    }

    #[tokio::test]
    async fn streaming_bridge_matches_tool_call_fixture_and_sanitizes_args() {
        let backend = fixture("streaming/backend_stream_tool_call.sse");
        let got = run_bridge_and_capture(&backend, DEFAULT_BACKEND_MODEL, Some("rid_TEST")).await;
        let expected = parse_expected_jsonl("streaming/expected_anthropic_tool_call.jsonl");
        assert_eq!(got, expected);
    }

    #[tokio::test]
    async fn streaming_bridge_matches_custom_tool_call_fixture() {
        let backend = fixture("streaming/backend_stream_custom_tool_call.sse");
        let got = run_bridge_and_capture(&backend, DEFAULT_BACKEND_MODEL, Some("rid_TEST")).await;
        let expected = parse_expected_jsonl("streaming/expected_anthropic_custom_tool_call.jsonl");
        assert_eq!(got, expected);
    }

    #[tokio::test]
    async fn streaming_bridge_matches_tool_search_call_fixture() {
        let backend = fixture("streaming/backend_stream_tool_search_call.sse");
        let got = run_bridge_and_capture(&backend, DEFAULT_BACKEND_MODEL, Some("rid_TEST")).await;
        let expected = parse_expected_jsonl("streaming/expected_anthropic_tool_search_call.jsonl");
        assert_eq!(got, expected);
    }

    #[tokio::test]
    async fn streaming_bridge_matches_local_shell_call_fixture() {
        let backend = fixture("streaming/backend_stream_local_shell_call.sse");
        let got = run_bridge_and_capture(&backend, DEFAULT_BACKEND_MODEL, Some("rid_TEST")).await;
        let expected = parse_expected_jsonl("streaming/expected_anthropic_local_shell_call.jsonl");
        assert_eq!(got, expected);
    }

    #[tokio::test]
    async fn streaming_bridge_counts_hosted_web_search_usage() {
        let backend = concat!(
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"web_search_call\",\"id\":\"ws_1\",\"status\":\"completed\",\"action\":{\"type\":\"search\",\"query\":\"rust release\",\"sources\":[{\"title\":\"Rust\",\"url\":\"https://www.rust-lang.org\"}]}}}\n\n",
            "event: response.output_text.delta\n",
            "data: {\"delta\":\"Search complete.\"}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"output\":[{\"type\":\"web_search_call\",\"id\":\"ws_1\",\"status\":\"completed\"}],\"usage\":{\"input_tokens\":1,\"output_tokens\":2,\"total_tokens\":3}}}\n\n",
        );
        let got = run_bridge_and_capture(backend, DEFAULT_BACKEND_MODEL, Some("rid_TEST")).await;
        let server_tool_uses = got
            .iter()
            .filter(|(event, data)| {
                event == "content_block_start" && data["content_block"]["type"] == "server_tool_use"
            })
            .count();
        let web_search_results = got
            .iter()
            .filter(|(event, data)| {
                event == "content_block_start"
                    && data["content_block"]["type"] == "web_search_tool_result"
            })
            .count();
        assert_eq!(server_tool_uses, 1);
        assert_eq!(web_search_results, 1);
        let result_block = got
            .iter()
            .find(|(event, data)| {
                event == "content_block_start"
                    && data["content_block"]["type"] == "web_search_tool_result"
            })
            .expect("web_search_tool_result")
            .1
            .clone();
        assert_eq!(
            result_block["content_block"]["content"][0]["url"],
            "https://www.rust-lang.org"
        );
        let message_delta = got
            .iter()
            .find(|(event, _data)| event == "message_delta")
            .expect("message_delta")
            .1
            .clone();
        assert_eq!(
            message_delta["usage"]["server_tool_use"]["web_search_requests"],
            1
        );
        assert_eq!(
            message_delta["usage"]["server_tool_use"]["web_fetch_requests"],
            0
        );
    }

    #[tokio::test]
    async fn streaming_bridge_surfaces_backend_failure_event() {
        let backend = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\"}\n\n",
            "event: response.in_progress\n",
            "data: {\"type\":\"response.in_progress\"}\n\n",
            "event: error\n",
            "data: {\"type\":\"error\",\"message\":\"model unavailable\"}\n\n",
            "event: response.failed\n",
            "data: {\"type\":\"response.failed\"}\n\n",
        );
        let got = run_bridge_and_capture(backend, DEFAULT_BACKEND_MODEL, Some("rid_TEST")).await;
        let error = got
            .iter()
            .find(|(event, _data)| event == "error")
            .expect("error event")
            .1
            .clone();
        assert_eq!(
            error["error"]["message"],
            "backend stream failed: error: model unavailable"
        );
    }
}
