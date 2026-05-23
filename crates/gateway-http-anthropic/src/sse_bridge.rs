#![forbid(unsafe_code)]

use axum::response::sse::Event;
use gateway_state::ToolCallStore;

#[derive(Default)]
pub(crate) struct StreamState {
    pub(crate) tool_block_started: bool,
    pub(crate) tool_call_id: Option<String>,
    pub(crate) tool_call_name: Option<String>,
    pub(crate) tool_args_buf: String,
    pub(crate) completed: bool,
}

pub(crate) fn tool_block_start(st: &StreamState) -> Event {
    let payload = serde_json::json!({
        "type": "content_block_start",
        "index": 1,
        "content_block": {
            "type": "tool_use",
            "id": st.tool_call_id.clone().unwrap_or_default(),
            "name": st.tool_call_name.clone().unwrap_or_default(),
            "input": {}
        }
    })
    .to_string();
    Event::default().event("content_block_start").data(payload)
}

pub(crate) fn tool_args_delta(delta: &str) -> Event {
    let payload = serde_json::json!({
        "type": "content_block_delta",
        "index": 1,
        "delta": { "type": "input_json_delta", "partial_json": delta }
    })
    .to_string();
    Event::default().event("content_block_delta").data(payload)
}

pub(crate) fn text_delta(text: &str) -> Event {
    let payload = serde_json::json!({
        "type": "content_block_delta",
        "index": 0,
        "delta": { "type": "text_delta", "text": text }
    })
    .to_string();
    Event::default().event("content_block_delta").data(payload)
}

pub(crate) fn finalize_message(st: &mut StreamState) -> Vec<Event> {
    st.completed = true;
    let mut out = Vec::new();
    out.push(
        Event::default()
            .event("content_block_stop")
            .data(serde_json::json!({"type":"content_block_stop","index":0}).to_string()),
    );
    if st.tool_block_started {
        out.push(
            Event::default()
                .event("content_block_stop")
                .data(serde_json::json!({"type":"content_block_stop","index":1}).to_string()),
        );
        out.push(
            Event::default().event("message_delta").data(
                serde_json::json!({
                    "type":"message_delta",
                    "delta":{"stop_reason":"tool_use","stop_sequence":null},
                    "usage":{"output_tokens":0}
                })
                .to_string(),
            ),
        );
    } else {
        out.push(
            Event::default().event("message_delta").data(
                serde_json::json!({
                    "type":"message_delta",
                    "delta":{"stop_reason":"end_turn","stop_sequence":null},
                    "usage":{"output_tokens":0}
                })
                .to_string(),
            ),
        );
    }
    out.push(
        Event::default()
            .event("message_stop")
            .data(serde_json::json!({"type":"message_stop"}).to_string()),
    );
    out
}

fn error_event(message: &str) -> Vec<Event> {
    let payload = serde_json::json!({
        "type": "error",
        "error": { "type": "backend_error", "message": message }
    })
    .to_string();
    vec![Event::default().event("error").data(payload)]
}

fn validate_tool_args_json_object(st: &StreamState) -> Result<(), String> {
    if !st.tool_block_started {
        return Ok(());
    }
    let trimmed = st.tool_args_buf.trim();
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
            Some(vec![text_delta(&text)])
        }
        "response.output_item.added" | "response.output_item.done" => {
            let v = serde_json::from_str::<serde_json::Value>(data).ok()?;
            let item = v.get("item")?;
            if item.get("type").and_then(|v| v.as_str()) != Some("function_call") {
                return None;
            }
            st.tool_call_id = item
                .get("call_id")
                .and_then(|s| s.as_str())
                .map(str::to_string);
            st.tool_call_name = item
                .get("name")
                .and_then(|s| s.as_str())
                .map(str::to_string);
            if let (Some(call_id), Some(name)) =
                (st.tool_call_id.as_deref(), st.tool_call_name.as_deref())
            {
                let _ = tool_calls.record_tool_call(call_id, name, request_id);
            }
            if st.tool_block_started {
                return None;
            }
            st.tool_block_started = true;
            Some(vec![tool_block_start(st)])
        }
        "response.function_call_arguments.delta" => {
            if !st.tool_block_started {
                return None;
            }
            let v = serde_json::from_str::<serde_json::Value>(data).ok()?;
            let delta = v.get("delta").and_then(|v| v.as_str())?;
            st.tool_args_buf.push_str(delta);
            Some(vec![tool_args_delta(delta)])
        }
        "response.completed" => match validate_tool_args_json_object(st) {
            Ok(()) => Some(finalize_message(st)),
            Err(err) => Some(error_event(&err)),
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_args_validation_requires_object() {
        let st = StreamState {
            tool_block_started: true,
            tool_args_buf: "[]".to_string(),
            ..StreamState::default()
        };
        let err = validate_tool_args_json_object(&st).expect_err("should reject non-object");
        assert_eq!(err, "tool_use.input must be a JSON object");
    }
}
