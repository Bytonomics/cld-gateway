#![forbid(unsafe_code)]

#[derive(Debug, Clone, serde::Deserialize)]
pub struct AnthropicMessagesRequest {
    pub model: String,
    pub messages: Vec<AnthropicMessage>,
    #[serde(default)]
    pub system: Vec<AnthropicSystemBlock>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default)]
    pub stop_sequences: Vec<String>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub top_p: Option<f64>,
    #[serde(default)]
    pub top_k: Option<u32>,
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
    #[serde(default)]
    pub tools: Vec<AnthropicToolDefinition>,
    #[serde(default)]
    pub tool_choice: Option<serde_json::Value>,
    #[serde(default)]
    pub thinking: Option<serde_json::Value>,
    #[serde(default)]
    pub output_config: Option<AnthropicOutputConfig>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct AnthropicOutputConfig {
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub format: Option<serde_json::Value>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct AnthropicToolDefinition {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct AnthropicSystemBlock {
    #[serde(rename = "type")]
    pub block_type: String,
    #[serde(default)]
    pub text: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct AnthropicMessage {
    pub role: String,
    pub content: AnthropicContent,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(untagged)]
pub enum AnthropicContent {
    Text(String),
    Blocks(Vec<AnthropicContentBlock>),
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct AnthropicContentBlock {
    #[serde(rename = "type")]
    pub block_type: String,

    // Common fields
    #[serde(default)]
    pub text: Option<String>,

    // Tool use
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub input: Option<serde_json::Value>,

    // Tool result
    #[serde(default)]
    pub tool_use_id: Option<String>,
    #[serde(default)]
    pub content: Option<serde_json::Value>,
    #[serde(default)]
    pub is_error: Option<bool>,

    // Image input (Anthropic uses { source: { type, media_type, data } })
    #[serde(default)]
    pub source: Option<AnthropicImageSource>,

    // Forward-compat: preserve unknown structured payloads without failing deserialization.
    #[serde(flatten)]
    pub extra: std::collections::BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct AnthropicImageSource {
    #[serde(rename = "type")]
    pub source_type: String,
    #[serde(default)]
    pub media_type: Option<String>,
    #[serde(default)]
    pub data: Option<String>,

    #[serde(flatten)]
    pub extra: std::collections::BTreeMap<String, serde_json::Value>,
}
