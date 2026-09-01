package dto

// Model is one entry in the GET /v1/models catalog response, ported from
// ClaudeGatewayModelsResponseItem (lib.rs:505-515).
type Model struct {
	ID             string  `json:"id"`
	Type           string  `json:"type"`
	Name           *string `json:"name,omitempty"`
	Description    *string `json:"description,omitempty"`
	MaxInputTokens *uint64 `json:"max_input_tokens,omitempty"`
}

// ModelList is the GET /v1/models response envelope (lib.rs:960).
type ModelList struct {
	Object string  `json:"object"`
	Data   []Model `json:"data"`
}
