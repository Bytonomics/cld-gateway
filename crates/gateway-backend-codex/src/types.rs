#![forbid(unsafe_code)]

use gateway_core::Secret;
use std::collections::HashMap;

#[derive(Clone)]
pub struct CodexBackendRequest {
    pub access_token: Secret<String>,
    pub account_id: String,
    /// Backend model id (post model-map resolution).
    pub model: String,
    /// System instructions (joined Anthropic `system[]` text blocks).
    pub instructions: String,
    /// Full conversation history encoded as Codex "Responses-like" items.
    pub input: Vec<serde_json::Value>,
    /// Function/tool definitions in the backend schema.
    pub tools: Vec<serde_json::Value>,
    /// Tool selection policy. Codex typically uses `"auto"`.
    pub tool_choice: String,
    /// Whether the backend may call tools in parallel.
    pub parallel_tool_calls: bool,
    /// Optional text controls (e.g. structured outputs via JSON schema).
    pub text: Option<serde_json::Value>,
    /// Optional reasoning controls (best-effort; backend-specific).
    pub reasoning: Option<serde_json::Value>,
    /// Required by the Codex backend contract: must be `false`.
    pub store: bool,
    /// If true, request a streaming SSE response.
    pub stream: bool,
    /// Optional include fields.
    pub include: Vec<String>,
    /// Optional sampling temperature (best-effort; backend may ignore).
    pub temperature: Option<f64>,
    /// Optional nucleus sampling value (best-effort; backend may ignore).
    pub top_p: Option<f64>,
    /// Optional client metadata (Codex-compatible).
    pub client_metadata: Option<HashMap<String, String>>,
}

#[derive(Debug, Clone)]
pub struct CodexBackendResponse {
    pub status: u16,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexUnaryDecoded {
    pub final_text: String,
    pub backend_model: Option<String>,
    pub tool_call: Option<CodexToolCall>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexToolCall {
    pub call_id: String,
    pub name: String,
    /// JSON-encoded arguments string (Codex protocol uses a JSON string, not an object).
    pub arguments: String,
}
