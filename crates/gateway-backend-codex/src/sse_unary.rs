#![forbid(unsafe_code)]

use crate::types::{CodexTokenUsage, CodexToolCall, CodexUnaryDecoded};
use bytes::Bytes;
use eventsource_stream::EventStreamError;
use eventsource_stream::Eventsource as _;
use futures_util::Stream;
use futures_util::StreamExt as _;
use serde::Deserialize;

#[derive(Debug, thiserror::Error)]
pub enum SseDecodeError {
    #[error("event stream error")]
    EventStream {
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
    E: std::fmt::Debug + std::fmt::Display + Send + Sync + 'static,
{
    let mut event_stream = Box::pin(byte_stream.eventsource());
    let mut last_text: Option<String> = None;
    let mut last_tool_call: Option<CodexToolCall> = None;
    let mut last_usage: Option<CodexTokenUsage> = None;

    while let Some(item) = event_stream.next().await {
        let event = item.map_err(|e: EventStreamError<E>| SseDecodeError::EventStream {
            source: Box::new(e),
        })?;
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
        if let Some(tool_call) = extract_tool_call_from_data(&event.event, data) {
            last_tool_call = Some(tool_call);
        }
        if let Some(text) = extract_last_text_from_data(data) {
            last_text = Some(text);
        }
    }

    let final_text = last_text.unwrap_or_default();
    if final_text.is_empty() && last_tool_call.is_none() {
        return Err(SseDecodeError::NoFinalText);
    }

    Ok(CodexUnaryDecoded {
        final_text,
        backend_model: None,
        token_usage: last_usage,
        tool_call: last_tool_call,
    })
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

fn extract_tool_call_from_data(event_name: &str, data: &str) -> Option<CodexToolCall> {
    // Codex backend uses typed SSE events; tool calls typically surface as a completed output item.
    if event_name != "response.output_item.done" && event_name != "response.output_item.added" {
        return None;
    }
    let value = serde_json::from_str::<serde_json::Value>(data).ok()?;
    // Two common shapes observed:
    // 1) { "type":"response.output_item.done", "item": { "type":"function_call", ... } }
    // 2) { "item": { "type":"function_call", ... } } (older/variant)
    let item = value
        .get("item")
        .cloned()
        .or_else(|| value.get("response").and_then(|r| r.get("item")).cloned())?;

    let item_type = item.get("type").and_then(|v| v.as_str())?;
    if item_type != "function_call" {
        return None;
    }
    Some(CodexToolCall {
        call_id: item.get("call_id")?.as_str()?.to_string(),
        name: item.get("name")?.as_str()?.to_string(),
        arguments: item.get("arguments")?.as_str()?.to_string(),
    })
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
        extract_last_text_from_data, extract_usage_from_completed_event, read_sse_to_completion,
    };
    use bytes::Bytes;
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
