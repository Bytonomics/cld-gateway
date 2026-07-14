#![forbid(unsafe_code)]

use crate::backend_error::parse_backend_failure_event;
use crate::output_text::{extract_text_from_data, parse_output_item_message_texts};
use crate::tool_calls::parse_output_item_tool_call;
use crate::types::{CodexBackendEvent, CodexTokenUsage, CodexToolCall, CodexUnaryDecoded};
use bytes::Bytes;
use eventsource_stream::EventStreamError;
use eventsource_stream::Eventsource as _;
use futures_util::Stream;
use futures_util::StreamExt as _;
use gateway_core::format_error_chain;
use serde::Deserialize;
use std::collections::BTreeSet;
use std::error::Error;

#[derive(Debug, thiserror::Error)]
pub enum SseDecodeError {
    #[error("event stream error: {message}")]
    EventStream {
        message: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("backend stream failed: {message}")]
    BackendFailed { message: String },
    #[error("no final text found in event stream; seen events: {events}")]
    NoFinalText { events: String },
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
    let event_stream = byte_stream.eventsource().map(|item| {
        item.map(|event| CodexBackendEvent {
            event: event.event,
            data: event.data,
        })
        .map_err(event_stream_decode_error)
    });
    read_backend_events_to_completion(event_stream).await
}

/// Read already-decoded backend events to completion and produce a single unary decoded value.
///
/// This is used by both HTTP/SSE and WebSocket transports so continuation mode cannot diverge from
/// the established response decoder.
///
/// # Errors
///
/// Returns an error if the event stream fails, the backend reports an error event, or no text/tool
/// output is found.
pub async fn read_backend_events_to_completion<S, E>(
    event_stream: S,
) -> Result<CodexUnaryDecoded, SseDecodeError>
where
    S: Stream<Item = Result<CodexBackendEvent, E>> + Send,
    E: Error + Send + Sync + 'static,
{
    let mut event_stream = Box::pin(event_stream);
    let mut final_text = String::new();
    let mut fallback_text: Option<String> = None;
    let mut saw_output_text_delta = false;
    let mut tool_calls: Vec<CodexToolCall> = Vec::new();
    let mut output_items: Vec<serde_json::Value> = Vec::new();
    let mut output_item_fingerprints = BTreeSet::new();
    let mut last_usage: Option<CodexTokenUsage> = None;
    let mut last_response_id: Option<String> = None;
    let mut web_search_call_ids = BTreeSet::new();
    let mut seen_events = Vec::new();

    while let Some(item) = event_stream.next().await {
        let event = item.map_err(generic_event_stream_decode_error)?;
        let data = event.data.trim();
        record_seen_event(&mut seen_events, &event.event);

        if data.is_empty() {
            continue;
        }

        // Common "terminator" token in some SSE protocols.
        if data == "[DONE]" {
            continue;
        }

        if let Some(message) = parse_backend_failure_event(&event.event, data) {
            return Err(SseDecodeError::BackendFailed { message });
        }

        if event.event == "response.completed"
            && let Some(usage) = extract_usage_from_completed_event(data)
        {
            last_usage = Some(usage);
        }
        if event.event == "response.completed" {
            last_response_id = extract_response_id_from_completed_event(data);
        }
        record_completed_web_search_call_ids(&mut web_search_call_ids, &event.event, data);
        match event.event.as_str() {
            "response.output_text.delta" => {
                if let Some(text) = extract_text_from_data(data) {
                    final_text.push_str(&text);
                    saw_output_text_delta = true;
                }
            }
            "response.output_text.done" => {
                if !saw_output_text_delta && let Some(text) = extract_text_from_data(data) {
                    final_text.push_str(&text);
                }
            }
            "response.output_item.added" | "response.output_item.done" => {
                collect_output_item(
                    &mut output_items,
                    &mut output_item_fingerprints,
                    &event.event,
                    data,
                );
                if let Some(tool_call) = parse_output_item_tool_call(&event.event, data) {
                    upsert_tool_call(&mut tool_calls, tool_call);
                }
                if !saw_output_text_delta {
                    for text in parse_output_item_message_texts(&event.event, data) {
                        final_text.push_str(&text);
                    }
                }
            }
            _ => {
                if event.event == "response.completed" {
                    collect_completed_output_items(
                        &mut output_items,
                        &mut output_item_fingerprints,
                        data,
                    );
                }
                if !is_tool_input_delta_event(&event.event)
                    && event.event != "response.completed"
                    && let Some(text) = extract_text_from_data(data)
                {
                    fallback_text = Some(text);
                }
            }
        }
    }

    if final_text.is_empty() {
        final_text = fallback_text.unwrap_or_default();
    }
    if final_text.is_empty() && tool_calls.is_empty() {
        return Err(SseDecodeError::NoFinalText {
            events: seen_events.join(","),
        });
    }
    if let Some(usage) = last_usage.as_mut() {
        usage.web_search_requests = saturating_u32_len(web_search_call_ids.len());
    }

    Ok(CodexUnaryDecoded {
        final_text,
        response_id: last_response_id,
        backend_model: None,
        token_usage: last_usage,
        output_items,
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
    #[serde(default)]
    id: Option<String>,
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
    #[serde(default)]
    total_tokens: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct CompletedInputTokensDetails {
    cached_tokens: i64,
}

#[derive(Debug, Deserialize)]
struct CompletedOutputTokensDetails {
    reasoning_tokens: i64,
}

#[must_use]
pub fn extract_usage_from_completed_event(data: &str) -> Option<CodexTokenUsage> {
    let value = serde_json::from_str::<serde_json::Value>(data).ok()?;
    // Observed shapes:
    // - { "type":"response.completed", "response": { "usage": {..} } }
    // - { "response": { "usage": {..} } } (variant)
    let env: CompletedEnvelope = serde_json::from_value(value).ok()?;
    let usage = env.response?.usage?;
    let total_tokens = usage
        .total_tokens
        .unwrap_or_else(|| usage.input_tokens.saturating_add(usage.output_tokens));
    Some(CodexTokenUsage {
        input_tokens: usage.input_tokens,
        cached_input_tokens: usage.input_tokens_details.map_or(0, |d| d.cached_tokens),
        output_tokens: usage.output_tokens,
        reasoning_output_tokens: usage
            .output_tokens_details
            .map_or(0, |d| d.reasoning_tokens),
        total_tokens,
        web_search_requests: 0,
    })
}

#[must_use]
pub fn extract_response_id_from_completed_event(data: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(data).ok()?;
    let env: CompletedEnvelope = serde_json::from_value(value).ok()?;
    env.response?.id
}

pub fn record_completed_web_search_call_ids(
    web_search_call_ids: &mut BTreeSet<String>,
    event_name: &str,
    data: &str,
) {
    for call_id in completed_web_search_call_ids(event_name, data) {
        web_search_call_ids.insert(call_id);
    }
}

#[must_use]
pub fn completed_web_search_call_ids(event_name: &str, data: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
        return Vec::new();
    };

    match event_name {
        "response.output_item.done" => value
            .get("item")
            .and_then(|item| completed_web_search_call_id(item, "output_item"))
            .into_iter()
            .collect(),
        "response.web_search_call.completed" => event_web_search_call_id(&value, event_name)
            .into_iter()
            .collect(),
        "response.completed" => value
            .pointer("/response/output")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
            .filter_map(|(index, item)| {
                completed_web_search_call_id(item, &format!("response_output_{index}"))
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn event_web_search_call_id(event: &serde_json::Value, fallback: &str) -> Option<String> {
    if event.get("type").and_then(serde_json::Value::as_str) != Some(fallback) {
        return None;
    }
    event
        .get("id")
        .or_else(|| event.get("call_id"))
        .or_else(|| event.get("item_id"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            event
                .get("output_index")
                .and_then(serde_json::Value::as_u64)
                .map(|index| format!("{fallback}:{index}"))
        })
        .or_else(|| Some(fallback.to_string()))
}

fn completed_web_search_call_id(item: &serde_json::Value, fallback: &str) -> Option<String> {
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

    item.get("id")
        .or_else(|| item.get("call_id"))
        .or_else(|| item.get("item_id"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .or_else(|| Some(fallback.to_string()))
}

fn saturating_u32_len(len: usize) -> u32 {
    u32::try_from(len).unwrap_or(u32::MAX)
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

fn generic_event_stream_decode_error<E>(source: E) -> SseDecodeError
where
    E: Error + Send + Sync + 'static,
{
    let message = format_error_chain(&source);
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

fn collect_output_item(
    output_items: &mut Vec<serde_json::Value>,
    fingerprints: &mut BTreeSet<String>,
    event_name: &str,
    data: &str,
) {
    if event_name != "response.output_item.done" {
        return;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
        return;
    };
    let Some(item) = value.get("item") else {
        return;
    };
    append_unique_output_item(output_items, fingerprints, item.clone());
}

fn collect_completed_output_items(
    output_items: &mut Vec<serde_json::Value>,
    fingerprints: &mut BTreeSet<String>,
    data: &str,
) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
        return;
    };
    let Some(items) = value
        .get("response")
        .and_then(|response| response.get("output"))
        .and_then(serde_json::Value::as_array)
    else {
        return;
    };
    for item in items {
        append_unique_output_item(output_items, fingerprints, item.clone());
    }
}

fn append_unique_output_item(
    output_items: &mut Vec<serde_json::Value>,
    fingerprints: &mut BTreeSet<String>,
    item: serde_json::Value,
) {
    let Ok(fingerprint) = serde_json::to_string(&item) else {
        return;
    };
    if fingerprints.insert(fingerprint) {
        output_items.push(item);
    }
}

fn record_seen_event(seen_events: &mut Vec<String>, event_name: &str) {
    if seen_events.iter().any(|seen| seen == event_name) {
        return;
    }
    seen_events.push(event_name.to_string());
}

#[cfg(test)]
mod tests {
    use super::{
        SseDecodeError, completed_web_search_call_ids, extract_response_id_from_completed_event,
        extract_usage_from_completed_event, format_event_stream_error, read_sse_to_completion,
    };
    use crate::types::CodexToolCallKind;
    use bytes::Bytes;
    use eventsource_stream::EventStreamError;
    use futures_util::stream;

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
    async fn sse_concatenates_output_text_deltas() {
        let sse = concat!(
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hello\"}\n\n",
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\" world\"}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":2,\"total_tokens\":3}}}\n\n",
        );

        let byte_stream = stream::iter(vec![Ok::<Bytes, std::io::Error>(Bytes::from(sse))]);
        let decoded = read_sse_to_completion(byte_stream).await.unwrap();
        assert_eq!(decoded.final_text, "hello world");
        assert_eq!(decoded.response_id, None);
    }

    #[tokio::test]
    async fn sse_decodes_output_item_message_text() {
        let sse = concat!(
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"message text\"}]}}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":2,\"total_tokens\":3}}}\n\n",
        );

        let byte_stream = stream::iter(vec![Ok::<Bytes, std::io::Error>(Bytes::from(sse))]);
        let decoded = read_sse_to_completion(byte_stream).await.unwrap();
        assert_eq!(decoded.final_text, "message text");
        assert_eq!(decoded.response_id, None);
        assert_eq!(decoded.output_items.len(), 1);
        assert_eq!(
            decoded.output_items[0]
                .get("type")
                .and_then(serde_json::Value::as_str),
            Some("message")
        );
    }

    #[tokio::test]
    async fn no_final_text_reports_seen_events() {
        let sse = concat!(
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":0,\"total_tokens\":1}}}\n\n",
        );

        let byte_stream = stream::iter(vec![Ok::<Bytes, std::io::Error>(Bytes::from(sse))]);
        let err = read_sse_to_completion(byte_stream).await.unwrap_err();
        assert!(matches!(err, SseDecodeError::NoFinalText { .. }));
        assert!(err.to_string().contains("response.completed"));
    }

    #[tokio::test]
    async fn backend_error_event_returns_concrete_failure() {
        let sse = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\"}\n\n",
            "event: error\n",
            "data: {\"type\":\"error\",\"message\":\"model unavailable\"}\n\n",
            "event: response.failed\n",
            "data: {\"type\":\"response.failed\"}\n\n",
        );

        let byte_stream = stream::iter(vec![Ok::<Bytes, std::io::Error>(Bytes::from(sse))]);
        let err = read_sse_to_completion(byte_stream).await.unwrap_err();
        assert!(matches!(err, SseDecodeError::BackendFailed { .. }));
        assert_eq!(
            err.to_string(),
            "backend stream failed: error: model unavailable"
        );
    }

    #[tokio::test]
    async fn response_failed_event_returns_concrete_failure() {
        let sse = concat!(
            "event: response.failed\n",
            "data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"message\":\"quota exceeded\"}}}\n\n",
        );

        let byte_stream = stream::iter(vec![Ok::<Bytes, std::io::Error>(Bytes::from(sse))]);
        let err = read_sse_to_completion(byte_stream).await.unwrap_err();
        assert!(matches!(err, SseDecodeError::BackendFailed { .. }));
        assert_eq!(
            err.to_string(),
            "backend stream failed: response.failed: quota exceeded"
        );
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
        assert_eq!(decoded.response_id, None);
        assert_eq!(decoded.output_items.len(), 1);
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

    #[tokio::test]
    async fn sse_ignores_hosted_web_search_call_and_keeps_final_text() {
        let sse = concat!(
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"web_search_call\",\"id\":\"ws_1\",\"status\":\"completed\",\"action\":{\"type\":\"search\",\"query\":\"rust release\"}}}\n\n",
            "event: response.output_text.delta\n",
            "data: {\"delta\":\"Search complete.\"}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":2,\"total_tokens\":3}}}\n\n",
        );

        let byte_stream = stream::iter(vec![Ok::<Bytes, std::io::Error>(Bytes::from(sse))]);
        let decoded = read_sse_to_completion(byte_stream).await.unwrap();
        assert_eq!(decoded.final_text, "Search complete.");
        assert_eq!(decoded.response_id, None);
        assert_eq!(decoded.output_items.len(), 1);
        assert!(decoded.tool_calls.is_empty());
        assert_eq!(decoded.token_usage.expect("usage").web_search_requests, 1);
    }

    #[tokio::test]
    async fn completed_output_items_are_captured_without_duplicates() {
        let sse = concat!(
            "event: response.output_item.done\n",
            "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"Read\",\"arguments\":\"{\\\"file_path\\\":\\\"/tmp/a.txt\\\"}\"}}\n\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_123\",\"output\":[{\"type\":\"function_call\",\"call_id\":\"call_1\",\"name\":\"Read\",\"arguments\":\"{\\\"file_path\\\":\\\"/tmp/a.txt\\\"}\"}],\"usage\":{\"input_tokens\":1,\"output_tokens\":0,\"total_tokens\":1}}}\n\n",
        );

        let byte_stream = stream::iter(vec![Ok::<Bytes, std::io::Error>(Bytes::from(sse))]);
        let decoded = read_sse_to_completion(byte_stream).await.unwrap();
        assert_eq!(decoded.response_id.as_deref(), Some("resp_123"));
        assert_eq!(decoded.output_items.len(), 1);
        assert_eq!(
            decoded.output_items[0]
                .get("type")
                .and_then(serde_json::Value::as_str),
            Some("function_call")
        );
    }

    #[test]
    fn response_id_extracts_from_completed_event() {
        let json = r#"{"type":"response.completed","response":{"id":"resp_123","usage":{"input_tokens":3,"output_tokens":5}}}"#;
        assert_eq!(
            extract_response_id_from_completed_event(json).as_deref(),
            Some("resp_123")
        );
    }

    #[test]
    fn completed_web_search_ids_extract_from_streaming_event_and_completed_output() {
        let streaming =
            r#"{"type":"response.web_search_call.completed","item_id":"ws_1","output_index":0}"#;
        assert_eq!(
            completed_web_search_call_ids("response.web_search_call.completed", streaming),
            vec!["ws_1".to_string()]
        );

        let completed = r#"{"type":"response.completed","response":{"output":[{"type":"web_search_call","id":"ws_2","status":"completed"}]}}"#;
        assert_eq!(
            completed_web_search_call_ids("response.completed", completed),
            vec!["ws_2".to_string()]
        );
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

    #[test]
    fn usage_derives_total_when_missing() {
        let json = r#"{"type":"response.completed","response":{"id":"r1","usage":{"input_tokens":3,"output_tokens":5}}}"#;
        let usage = extract_usage_from_completed_event(json).expect("usage present");
        assert_eq!(usage.input_tokens, 3);
        assert_eq!(usage.output_tokens, 5);
        assert_eq!(usage.total_tokens, 8);
    }
}
