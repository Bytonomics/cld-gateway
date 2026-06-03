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
    pub token_usage: Option<CodexTokenUsage>,
    pub tool_calls: Vec<CodexToolCall>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodexTokenUsage {
    pub input_tokens: i64,
    pub cached_input_tokens: i64,
    pub output_tokens: i64,
    pub reasoning_output_tokens: i64,
    pub total_tokens: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexToolCall {
    pub call_id: String,
    pub name: String,
    /// JSON-object encoded Anthropic-compatible `tool_use.input`.
    pub arguments: String,
    pub kind: CodexToolCallKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexToolCallKind {
    Function,
    Custom,
    ToolSearch,
    LocalShell,
}

impl CodexToolCallKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Function => "function_call",
            Self::Custom => "custom_tool_call",
            Self::ToolSearch => "tool_search_call",
            Self::LocalShell => "local_shell_call",
        }
    }

    #[must_use]
    pub fn output_type(self) -> &'static str {
        match self {
            Self::Function | Self::LocalShell => "function_call_output",
            Self::Custom => "custom_tool_call_output",
            Self::ToolSearch => "tool_search_output",
        }
    }
}

impl std::str::FromStr for CodexToolCallKind {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "function_call" => Ok(Self::Function),
            "custom_tool_call" => Ok(Self::Custom),
            "tool_search_call" => Ok(Self::ToolSearch),
            "local_shell_call" => Ok(Self::LocalShell),
            _ => Err(()),
        }
    }
}
