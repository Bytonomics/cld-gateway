// Package dto holds request/response data-transfer types for the Anthropic
// Messages API surface. Field-for-field port of
// crates/gateway-http-anthropic/src/types.rs; JSON tags and optionality
// (serde `#[serde(default)]` -> Go pointer/omitempty) mirror the Rust
// struct exactly.
package dto

import (
	"encoding/json"

	"github.com/SmrutAI/pedantigo/v2/validator"
)

// MessagesRequest ports AnthropicMessagesRequest (types.rs:3-33).
type MessagesRequest struct {
	Model             string             `json:"model" validate:"required"`
	Messages          []Message          `json:"messages" validate:"required,min=1,dive"`
	System            []SystemBlock      `json:"system,omitempty" validate:"omitempty,dive"`
	Stream            bool               `json:"stream,omitempty"`
	StopSequences     []string           `json:"stop_sequences,omitempty"`
	MaxTokens         *uint32            `json:"max_tokens,omitempty" validate:"omitempty,gt=0"`
	Temperature       *float64           `json:"temperature,omitempty"`
	TopP              *float64           `json:"top_p,omitempty"`
	TopK              *uint32            `json:"top_k,omitempty"`
	Metadata          any                `json:"metadata,omitempty"`
	Tools             []Tool             `json:"tools,omitempty" validate:"omitempty,dive"`
	ToolChoice        *ToolChoice        `json:"tool_choice,omitempty"`
	Thinking          any                `json:"thinking,omitempty"`
	ContextManagement *ContextManagement `json:"context_management,omitempty"`
	OutputConfig      *OutputConfig      `json:"output_config,omitempty"`
}

var _ = validator.Register(validator.New[MessagesRequest]())

// ContextManagement ports AnthropicContextManagement (types.rs:35-39).
type ContextManagement struct {
	Edits []ContextEdit `json:"edits,omitempty" validate:"omitempty,dive"`
}

// ContextEdit ports AnthropicContextEdit (types.rs:41-55).
type ContextEdit struct {
	EditType        string            `json:"type" validate:"required"`
	Trigger         *ContextThreshold `json:"trigger,omitempty"`
	Keep            any               `json:"keep,omitempty"`
	ClearAtLeast    *ContextThreshold `json:"clear_at_least,omitempty"`
	ExcludeTools    []string          `json:"exclude_tools,omitempty"`
	ClearToolInputs bool              `json:"clear_tool_inputs,omitempty"`
}

// ContextThreshold ports AnthropicContextThreshold (types.rs:57-62).
type ContextThreshold struct {
	ThresholdType string `json:"type" validate:"required"`
	Value         uint64 `json:"value" validate:"required"`
}

// OutputConfig ports AnthropicOutputConfig (types.rs:64-70).
type OutputConfig struct {
	Effort *string `json:"effort,omitempty"`
	Format any     `json:"format,omitempty"`
}

// Tool ports AnthropicToolDefinition (types.rs:72-89). The Rust struct
// flattens unknown fields into `extra`; toolJSON below mirrors that via
// custom (Un)MarshalJSON.
type Tool struct {
	Name           string                     `json:"name" validate:"required"`
	ToolType       *string                    `json:"type,omitempty"`
	Description    *string                    `json:"description,omitempty"`
	InputSchema    any                        `json:"input_schema,omitempty"`
	AllowedDomains []string                   `json:"allowed_domains,omitempty"`
	BlockedDomains []string                   `json:"blocked_domains,omitempty"`
	MaxUses        *uint32                    `json:"max_uses,omitempty" validate:"omitempty,gt=0"`
	Extra          map[string]json.RawMessage `json:"-"`
}

type toolShadow struct {
	Name           string   `json:"name"`
	ToolType       *string  `json:"type,omitempty"`
	Description    *string  `json:"description,omitempty"`
	InputSchema    any      `json:"input_schema,omitempty"`
	AllowedDomains []string `json:"allowed_domains,omitempty"`
	BlockedDomains []string `json:"blocked_domains,omitempty"`
	MaxUses        *uint32  `json:"max_uses,omitempty"`
}

// knownToolKeys are the JSON keys owned by toolShadow; anything else lands
// in Extra (mirrors serde's #[serde(flatten)] on AnthropicToolDefinition).
var knownToolKeys = map[string]bool{
	"name": true, "type": true, "description": true, "input_schema": true,
	"allowed_domains": true, "blocked_domains": true, "max_uses": true,
}

func (t *Tool) UnmarshalJSON(data []byte) error {
	var shadow toolShadow
	if err := json.Unmarshal(data, &shadow); err != nil {
		return err
	}
	raw := map[string]json.RawMessage{}
	if err := json.Unmarshal(data, &raw); err != nil {
		return err
	}
	extra := map[string]json.RawMessage{}
	for k, v := range raw {
		if !knownToolKeys[k] {
			extra[k] = v
		}
	}
	t.Name = shadow.Name
	t.ToolType = shadow.ToolType
	t.Description = shadow.Description
	t.InputSchema = shadow.InputSchema
	t.AllowedDomains = shadow.AllowedDomains
	t.BlockedDomains = shadow.BlockedDomains
	t.MaxUses = shadow.MaxUses
	t.Extra = extra
	return nil
}

func (t Tool) MarshalJSON() ([]byte, error) {
	out := map[string]json.RawMessage{}
	for k, v := range t.Extra {
		out[k] = v
	}
	shadow := toolShadow{
		Name: t.Name, ToolType: t.ToolType, Description: t.Description,
		InputSchema: t.InputSchema, AllowedDomains: t.AllowedDomains,
		BlockedDomains: t.BlockedDomains, MaxUses: t.MaxUses,
	}
	shadowJSON, err := json.Marshal(shadow)
	if err != nil {
		return nil, err
	}
	var shadowMap map[string]json.RawMessage
	if err := json.Unmarshal(shadowJSON, &shadowMap); err != nil {
		return nil, err
	}
	for k, v := range shadowMap {
		out[k] = v
	}
	return json.Marshal(out)
}

// ToolChoice ports AnthropicMessagesRequest.tool_choice, which Rust keeps
// as an opaque `Option<serde_json::Value>` (types.rs:26-27). Go keeps it
// equally opaque: round-trips whatever JSON value the client sent.
type ToolChoice struct {
	Raw json.RawMessage
}

func (tc *ToolChoice) UnmarshalJSON(data []byte) error {
	tc.Raw = append(json.RawMessage(nil), data...)
	return nil
}

func (tc ToolChoice) MarshalJSON() ([]byte, error) {
	if tc.Raw == nil {
		return []byte("null"), nil
	}
	return tc.Raw, nil
}

// SystemBlock ports AnthropicSystemBlock (types.rs:91-97).
type SystemBlock struct {
	BlockType string  `json:"type" validate:"required"`
	Text      *string `json:"text,omitempty"`
}

// Message ports AnthropicMessage (types.rs:99-103).
type Message struct {
	Role    string  `json:"role" validate:"required"`
	Content Content `json:"content" validate:"required"`
}

// Content ports the untagged AnthropicContent enum (types.rs:105-110):
// either a plain string or an array of content blocks.
type Content struct {
	Text   *string
	Blocks []ContentBlock
}

func (c *Content) UnmarshalJSON(data []byte) error {
	var text string
	if err := json.Unmarshal(data, &text); err == nil {
		c.Text = &text
		c.Blocks = nil
		return nil
	}
	var blocks []ContentBlock
	if err := json.Unmarshal(data, &blocks); err != nil {
		return err
	}
	c.Blocks = blocks
	c.Text = nil
	return nil
}

func (c Content) MarshalJSON() ([]byte, error) {
	if c.Text != nil {
		return json.Marshal(*c.Text)
	}
	return json.Marshal(c.Blocks)
}

// ContentBlock ports AnthropicContentBlock (types.rs:112-144).
type ContentBlock struct {
	BlockType string                     `json:"type" validate:"required"`
	Text      *string                    `json:"text,omitempty"`
	ID        *string                    `json:"id,omitempty"`
	Name      *string                    `json:"name,omitempty"`
	Input     any                        `json:"input,omitempty"`
	ToolUseID *string                    `json:"tool_use_id,omitempty"`
	Content   any                        `json:"content,omitempty"`
	IsError   *bool                      `json:"is_error,omitempty"`
	Source    *ImageSource               `json:"source,omitempty"`
	Extra     map[string]json.RawMessage `json:"-"`
}

type contentBlockShadow struct {
	BlockType string       `json:"type"`
	Text      *string      `json:"text,omitempty"`
	ID        *string      `json:"id,omitempty"`
	Name      *string      `json:"name,omitempty"`
	Input     any          `json:"input,omitempty"`
	ToolUseID *string      `json:"tool_use_id,omitempty"`
	Content   any          `json:"content,omitempty"`
	IsError   *bool        `json:"is_error,omitempty"`
	Source    *ImageSource `json:"source,omitempty"`
}

var knownContentBlockKeys = map[string]bool{
	"type": true, "text": true, "id": true, "name": true, "input": true,
	"tool_use_id": true, "content": true, "is_error": true, "source": true,
}

func (b *ContentBlock) UnmarshalJSON(data []byte) error {
	var shadow contentBlockShadow
	if err := json.Unmarshal(data, &shadow); err != nil {
		return err
	}
	raw := map[string]json.RawMessage{}
	if err := json.Unmarshal(data, &raw); err != nil {
		return err
	}
	extra := map[string]json.RawMessage{}
	for k, v := range raw {
		if !knownContentBlockKeys[k] {
			extra[k] = v
		}
	}
	b.BlockType = shadow.BlockType
	b.Text = shadow.Text
	b.ID = shadow.ID
	b.Name = shadow.Name
	b.Input = shadow.Input
	b.ToolUseID = shadow.ToolUseID
	b.Content = shadow.Content
	b.IsError = shadow.IsError
	b.Source = shadow.Source
	b.Extra = extra
	return nil
}

func (b ContentBlock) MarshalJSON() ([]byte, error) {
	out := map[string]json.RawMessage{}
	for k, v := range b.Extra {
		out[k] = v
	}
	shadow := contentBlockShadow{
		BlockType: b.BlockType, Text: b.Text, ID: b.ID, Name: b.Name,
		Input: b.Input, ToolUseID: b.ToolUseID, Content: b.Content,
		IsError: b.IsError, Source: b.Source,
	}
	shadowJSON, err := json.Marshal(shadow)
	if err != nil {
		return nil, err
	}
	var shadowMap map[string]json.RawMessage
	if err := json.Unmarshal(shadowJSON, &shadowMap); err != nil {
		return nil, err
	}
	for k, v := range shadowMap {
		out[k] = v
	}
	return json.Marshal(out)
}

// ImageSource ports AnthropicImageSource (types.rs:146-157).
type ImageSource struct {
	SourceType string                     `json:"type" validate:"required"`
	MediaType  *string                    `json:"media_type,omitempty"`
	Data       *string                    `json:"data,omitempty"`
	Extra      map[string]json.RawMessage `json:"-"`
}

type imageSourceShadow struct {
	SourceType string  `json:"type"`
	MediaType  *string `json:"media_type,omitempty"`
	Data       *string `json:"data,omitempty"`
}

var knownImageSourceKeys = map[string]bool{"type": true, "media_type": true, "data": true}

func (s *ImageSource) UnmarshalJSON(data []byte) error {
	var shadow imageSourceShadow
	if err := json.Unmarshal(data, &shadow); err != nil {
		return err
	}
	raw := map[string]json.RawMessage{}
	if err := json.Unmarshal(data, &raw); err != nil {
		return err
	}
	extra := map[string]json.RawMessage{}
	for k, v := range raw {
		if !knownImageSourceKeys[k] {
			extra[k] = v
		}
	}
	s.SourceType = shadow.SourceType
	s.MediaType = shadow.MediaType
	s.Data = shadow.Data
	s.Extra = extra
	return nil
}

func (s ImageSource) MarshalJSON() ([]byte, error) {
	out := map[string]json.RawMessage{}
	for k, v := range s.Extra {
		out[k] = v
	}
	shadow := imageSourceShadow{SourceType: s.SourceType, MediaType: s.MediaType, Data: s.Data}
	shadowJSON, err := json.Marshal(shadow)
	if err != nil {
		return nil, err
	}
	var shadowMap map[string]json.RawMessage
	if err := json.Unmarshal(shadowJSON, &shadowMap); err != nil {
		return nil, err
	}
	for k, v := range shadowMap {
		out[k] = v
	}
	return json.Marshal(out)
}

// MessagesResponse is the unary /v1/messages response shape built in Rust
// by build_unary_messages_response (crates/gateway-http-anthropic/src/lib.rs:1549-1618).
// types.rs itself has no response struct (Rust builds raw serde_json::Value);
// this is the first typed port of that shape.
type MessagesResponse struct {
	ID                string         `json:"id"`
	Type              string         `json:"type"`
	Role              string         `json:"role"`
	Model             string         `json:"model"`
	Content           []ContentBlock `json:"content"`
	StopReason        *string        `json:"stop_reason"`
	StopSequence      *string        `json:"stop_sequence"`
	Usage             Usage          `json:"usage"`
	ContextManagement map[string]any `json:"context_management,omitempty"`
}

// Usage ports the usage object built at lib.rs:1555-1573.
type Usage struct {
	InputTokens   int            `json:"input_tokens"`
	OutputTokens  int            `json:"output_tokens"`
	ServerToolUse *ServerToolUse `json:"server_tool_use,omitempty"`
}

// ServerToolUse ports the server_tool_use object built at lib.rs:1566-1569.
type ServerToolUse struct {
	WebSearchRequests int `json:"web_search_requests"`
	WebFetchRequests  int `json:"web_fetch_requests"`
}
