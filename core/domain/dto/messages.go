// Package dto holds request/response data-transfer types for the Anthropic
// Messages API surface. Field-for-field port of
// crates/gateway-http-anthropic/src/types.rs; JSON tags and optionality
// (serde `#[serde(default)]` -> Go pointer/omitempty) mirror the Rust
// struct exactly.
package dto

import (
	"encoding/json"
	"fmt"

	"github.com/SmrutAI/pedantigo/v2/validator"
)

var (
	_ validator.WalkerDecoder = (*Content)(nil)
	_ validator.WalkerDecoder = (*ToolChoice)(nil)
	_ validator.WalkerDecoder = (*Tool)(nil)
	_ validator.WalkerDecoder = (*ContentBlock)(nil)
	_ validator.WalkerDecoder = (*ImageSource)(nil)
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
	Name           string   `json:"name" validate:"required"`
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

func (t *Tool) DecodeWalk(decoded any, recurse func(dst any, decoded any) error) error {
	obj, ok := decoded.(map[string]any)
	if !ok {
		return fmt.Errorf("tool must be a JSON object, got %T", decoded)
	}
	var shadow toolShadow
	if err := recurse(&shadow, decoded); err != nil {
		return err
	}
	extra := map[string]json.RawMessage{}
	for k, val := range obj {
		if knownToolKeys[k] {
			continue
		}
		raw, err := json.Marshal(val)
		if err != nil {
			return err
		}
		extra[k] = raw
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

func (tc *ToolChoice) DecodeWalk(decoded any, recurse func(dst any, decoded any) error) error {
	raw, err := json.Marshal(decoded)
	if err != nil {
		return err
	}
	tc.Raw = raw
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

func (c *Content) DecodeWalk(decoded any, recurse func(dst any, decoded any) error) error {
	switch v := decoded.(type) {
	case string:
		c.Text = &v
		c.Blocks = nil
		return nil
	case []any:
		c.Text = nil
		return recurse(&c.Blocks, v)
	default:
		return fmt.Errorf("content must be a string or an array of content blocks, got %T", decoded)
	}
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
	BlockType string       `json:"type" validate:"required"`
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

func (b *ContentBlock) DecodeWalk(decoded any, recurse func(dst any, decoded any) error) error {
	obj, ok := decoded.(map[string]any)
	if !ok {
		return fmt.Errorf("content block must be a JSON object, got %T", decoded)
	}
	var shadow contentBlockShadow
	if err := recurse(&shadow, decoded); err != nil {
		return err
	}
	extra := map[string]json.RawMessage{}
	for k, val := range obj {
		if knownContentBlockKeys[k] {
			continue
		}
		raw, err := json.Marshal(val)
		if err != nil {
			return err
		}
		extra[k] = raw
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
	SourceType string  `json:"type" validate:"required"`
	MediaType  *string `json:"media_type,omitempty"`
	Data       *string `json:"data,omitempty"`
}

var knownImageSourceKeys = map[string]bool{"type": true, "media_type": true, "data": true}

func (s *ImageSource) DecodeWalk(decoded any, recurse func(dst any, decoded any) error) error {
	obj, ok := decoded.(map[string]any)
	if !ok {
		return fmt.Errorf("image source must be a JSON object, got %T", decoded)
	}
	var shadow imageSourceShadow
	if err := recurse(&shadow, decoded); err != nil {
		return err
	}
	extra := map[string]json.RawMessage{}
	for k, val := range obj {
		if knownImageSourceKeys[k] {
			continue
		}
		raw, err := json.Marshal(val)
		if err != nil {
			return err
		}
		extra[k] = raw
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

// Warning is a non-fatal, structured signal attached to an otherwise
// successful response, e.g. when the gateway fell back to sending full
// conversation history instead of an incremental delta. Code is a stable,
// machine-readable identifier (e.g. "delta_calculation_failed"); Message is
// the human-readable explanation, branded to identify cld-gateway as the
// source, matching the branding convention used for error messages.
type Warning struct {
	Code    string `json:"code"`
	Message string `json:"message"`
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
	Warnings          []Warning      `json:"warnings,omitempty"`
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
