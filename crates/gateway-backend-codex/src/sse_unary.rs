#![forbid(unsafe_code)]

use crate::tool_calls::parse_output_item_tool_call;
use crate::types::{CodexTokenUsage, CodexToolCall, CodexUnaryDecoded};
use bytes::Bytes;
use eventsource_stream::EventStreamError;
use eventsource_stream::Eventsource as _;
use futures_util::Stream;
use futures_util::StreamExt as _;
use gateway_core::format_error_chain;
use serde::Deserialize;
use std::error::Error;

#[derive(Debug, thiserror::Error)]
pub enum SseDecodeError {
    #[error("event stream error: {message}")]
    EventStream {
        message: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("no final text found in event stream")]
    NoFinalText,
}

/// Read a backend `text/event-stream` response to completion and produce a single unary decoded value.
///
/// Strategy (tolerant, per plan):
/// - Parse SSE framing via `eventsource-stream`.
/// - For each event's `data`, attempt to extract a "text-like" payload.
/// - Return the last extracted text as `final_text`.
///
/// # Errors
///
/// Returns an error if the SSE framing/transport fails, or if the stream completes without any
/// extractable text payload.
pub async fn read_sse_to_completion<S, E>(
    byte_stream: S,
) -> Result<CodexUnaryDecoded, SseDecodeError>
where
    S: Stream<Item = Result<Bytes, E>> + Send,
    E: Error + Send + Sync + 'static,
{
    let mut event_stream = Box::pin(byte_stream.eventsource());
    let mut last_text: Option<String> = None;
    let mut tool_calls: Vec<CodexToolCall> = Vec::new();
    let mut last_usage: Option<CodexTokenUsage> = None;

    while let Some(item) = event_stream.next().await {
        let event = item.map_err(event_stream_decode_error)?;
        let data = event.data.trim();

        if data.is_empty() {
            continue;
        }

        // Common "terminator" token in some SSE protocols.
        if data == "[DONE]" {
            continue;
        }

        if event.event == "response.completed"
            && let Some(usage) = extract_usage_from_completed_event(data)
        {
            last_usage = Some(usage);
        }
        if let Some(tool_call) = parse_output_item_tool_call(&event.event, data) {
            upsert_tool_call(&mut tool_calls, tool_call);
        }
        if !is_tool_input_delta_event(&event.event)
            && let Some(text) = extract_last_text_from_data(data)
        {
            last_text = Some(text);
        }
    }

    let final_text = last_text.unwrap_or_default();
    if final_text.is_empty() && tool_calls.is_empty() {
        return Err(SseDecodeError::NoFinalText);
    }

    Ok(CodexUnaryDecoded {
        final_text,
        backend_model: None,
        token_usage: last_usage,
        tool_calls,
    })
}

fn is_tool_input_delta_event(event_name: &str) -> bool {
    matches!(
        event_name,
        "response.function_call_arguments.delta" | "response.custom_tool_call_input.delta"
    )
}

#[derive(Debug, Deserialize)]
struct CompletedEnvelope {
    response: Option<CompletedResponse>,
}

#[derive(Debug, Deserialize)]
struct CompletedResponse {
    usage: Option<CompletedUsage>,
}

#[derive(Debug, Deserialize)]
struct CompletedUsage {
    input_tokens: i64,
    #[serde(default)]
    input_tokens_details: Option<CompletedInputTokensDetails>,
    output_tokens: i64,
    #[serde(default)]
    output_tokens_details: Option<CompletedOutputTokensDetails>,
    total_tokens: i64,
}

#[derive(Debug, Deserialize)]
struct CompletedInputTokensDetails {
    cached_tokens: i64,
}

#[derive(Debug, Deserialize)]
struct CompletedOutputTokensDetails {
    reasoning_tokens: i64,
}

fn extract_usage_from_completed_event(data: &str) -> Option<CodexTokenUsage> {
    let value = serde_json::from_str::<serde_json::Value>(data).ok()?;
    // Observed shapes:
    // - { "type":"response.completed", "response": { "usage": {..} } }
    // - { "response": { "usage": {..} } } (variant)
    let env: CompletedEnvelope = serde_json::from_value(value).ok()?;
    let usage = env.response?.usage?;
    Some(CodexTokenUsage {
        input_tokens: usage.input_tokens,
        cached_input_tokens: usage.input_tokens_details.map_or(0, |d| d.cached_tokens),
        output_tokens: usage.output_tokens,
        reasoning_output_tokens: usage
            .output_tokens_details
            .map_or(0, |d| d.reasoning_tokens),
        total_tokens: usage.total_tokens,
    })
}

fn event_stream_decode_error<E>(source: EventStreamError<E>) -> SseDecodeError
where
    E: Error + Send + Sync + 'static,
{
    let message = format_event_stream_error(&source);
    SseDecodeError::EventStream {
        message,
        source: Box::new(source),
    }
}

#[must_use]
pub fn format_event_stream_error<E>(error: &EventStreamError<E>) -> String
where
    E: Error + Send + Sync + 'static,
{
    match error {
        EventStreamError::Transport(source) => {
            format!("Transport error: {}", format_error_chain(source))
        }
        EventStreamError::Utf8(_) | EventStreamError::Parser(_) => format_error_chain(error),
    }
}

fn upsert_tool_call(tool_calls: &mut Vec<CodexToolCall>, tool_call: CodexToolCall) {
    if let Some(existing) = tool_calls
        .iter_mut()
        .find(|existing| existing.call_id == tool_call.call_id)
    {
        *existing = tool_call;
        return;
    }
    tool_calls.push(tool_call);
}

fn extract_last_text_from_data(data: &str) -> Option<String> {
    // Be permissive: treat non-JSON as plaintext output.
    let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
        return Some(data.to_string());
    };

    let mut last: Option<String> = None;
    extract_last_text_from_value(&value, &mut last);
    last
}

fn extract_last_text_from_value(value: &serde_json::Value, last: &mut Option<String>) {
    match value {
        serde_json::Value::Object(map) => {
            // Heuristic: prefer direct "text" fields if present.
            if let Some(serde_json::Value::String(text)) = map.get("text") {
                *last = Some(text.clone());
            }
            if let Some(serde_json::Value::String(delta)) = map.get("delta") {
                *last = Some(delta.clone());
            }

            // Special-case: some protocols embed assistant text in `content: [{text: "..."}]`.
            if let Some(content) = map.get("content") {
                extract_last_text_from_value(content, last);
            }

            // Walk nested structures, but only to find "text"/"delta"/content-like fields, not arbitrary strings.
            for (k, v) in map {
                if k == "text" || k == "delta" || k == "content" {
                    continue;
                }
                extract_last_text_from_value(v, last);
            }
        }
        serde_json::Value::Array(items) => {
            for v in items {
                extract_last_text_from_value(v, last);
            }
        }
        // Don't treat arbitrary JSON scalars as content; only extract from known/likely fields.
        serde_json::Value::String(_)
        | serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::{
        extract_last_text_from_data, extract_usage_from_completed_event, format_event_stream_error,
        read_sse_to_completion,
    };
    use crate::types::CodexToolCallKind;
    use bytes::Bytes;
    use eventsource_stream::EventStreamError;
    use futures_util::stream;

    #[test]
    fn plaintext_data_extracts_as_text() {
        let got = extract_last_text_from_data("hello");
        assert_eq!(got.as_deref(), Some("hello"));
    }

    #[test]
    fn json_data_extracts_text_field() {
        let got = extract_last_text_from_data(r#"{"text":"hi"}"#);
        assert_eq!(got.as_deref(), Some("hi"));
    }

    #[test]
    fn event_stream_transport_error_includes_underlying_error() {
        let err = EventStreamError::Transport(std::io::Error::other("socket closed"));
        assert_eq!(
            format_event_stream_error(&err),
            "Transport error: socket closed"
        );
    }

    #[tokio::test]
    async fn sse_multievent_last_text_wins() {
        let sse = concat!(
            "event: message\n",
            "data: first\n\n",
            "event: message\n",
            "data: second\n\n"
        );

        let byte_stream = stream::iter(vec![Ok::<Bytes, std::io::Error>(Bytes::from(sse))]);
        let decoded = read_sse_to_completion(byte_stream).await.unwrap();
        assert_eq!(decoded.final_text, "second");
    }

    #[tokio::test]
    async fn sse_decodes_function_tool_call_without_text() {
        let sse = concat!(
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"Read\",\"arguments\":\"{\\\"file_path\\\":\\\"/tmp/a.txt\\\"}\"}}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":0,\"total_tokens\":1}}}\n\n",
        );

        let byte_stream = stream::iter(vec![Ok::<Bytes, std::io::Error>(Bytes::from(sse))]);
        let decoded = read_sse_to_completion(byte_stream).await.unwrap();
        assert_eq!(decoded.final_text, "");
        assert_eq!(decoded.tool_calls.len(), 1);
        assert_eq!(decoded.tool_calls[0].kind, CodexToolCallKind::Function);
        assert_eq!(decoded.tool_calls[0].name, "Read");
        assert_eq!(
            decoded.tool_calls[0].arguments,
            r#"{"file_path":"/tmp/a.txt"}"#
        );
    }

    #[tokio::test]
    async fn sse_decodes_custom_tool_call_without_text() {
        let sse = concat!(
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"custom_tool_call\",\"call_id\":\"call_2\",\"name\":\"apply_patch\",\"input\":\"*** Begin Patch\\n*** End Patch\\n\"}}\n\n",
        );

        let byte_stream = stream::iter(vec![Ok::<Bytes, std::io::Error>(Bytes::from(sse))]);
        let decoded = read_sse_to_completion(byte_stream).await.unwrap();
        assert_eq!(decoded.tool_calls.len(), 1);
        assert_eq!(decoded.tool_calls[0].kind, CodexToolCallKind::Custom);
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&decoded.tool_calls[0].arguments).unwrap(),
            serde_json::json!({"input":"*** Begin Patch\n*** End Patch\n"})
        );
    }

    #[tokio::test]
    async fn sse_decodes_tool_search_call_without_text() {
        let sse = concat!(
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"tool_search_call\",\"call_id\":\"call_3\",\"execution\":\"client\",\"arguments\":{\"query\":\"Read\"}}}\n\n",
        );

        let byte_stream = stream::iter(vec![Ok::<Bytes, std::io::Error>(Bytes::from(sse))]);
        let decoded = read_sse_to_completion(byte_stream).await.unwrap();
        assert_eq!(decoded.tool_calls.len(), 1);
        assert_eq!(decoded.tool_calls[0].kind, CodexToolCallKind::ToolSearch);
        assert_eq!(decoded.tool_calls[0].name, "tool_search");
        assert_eq!(decoded.tool_calls[0].arguments, r#"{"query":"Read"}"#);
    }

    #[tokio::test]
    async fn sse_decodes_local_shell_call_without_text() {
        let sse = concat!(
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"local_shell_call\",\"call_id\":\"call_4\",\"status\":\"completed\",\"action\":{\"type\":\"exec\",\"command\":[\"echo\",\"hi\"]}}}\n\n",
        );

        let byte_stream = stream::iter(vec![Ok::<Bytes, std::io::Error>(Bytes::from(sse))]);
        let decoded = read_sse_to_completion(byte_stream).await.unwrap();
        assert_eq!(decoded.tool_calls.len(), 1);
        assert_eq!(decoded.tool_calls[0].kind, CodexToolCallKind::LocalShell);
        assert_eq!(decoded.tool_calls[0].name, "local_shell");
        let args = serde_json::from_str::<serde_json::Value>(&decoded.tool_calls[0].arguments)
            .expect("json object");
        assert_eq!(args["status"], "completed");
        assert_eq!(args["action"]["command"][1], "hi");
    }

    #[test]
    fn usage_extracts_from_completed_event() {
        let json = r#"{"type":"response.completed","response":{"id":"r1","usage":{"input_tokens":3,"input_tokens_details":{"cached_tokens":1},"output_tokens":5,"output_tokens_details":{"reasoning_tokens":2},"total_tokens":8}}}"#;
        let usage = extract_usage_from_completed_event(json).expect("usage present");
        assert_eq!(usage.input_tokens, 3);
        assert_eq!(usage.cached_input_tokens, 1);
        assert_eq!(usage.output_tokens, 5);
        assert_eq!(usage.reasoning_output_tokens, 2);
        assert_eq!(usage.total_tokens, 8);
    }
}
