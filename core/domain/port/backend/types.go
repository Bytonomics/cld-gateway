package backend

import (
	"encoding/json"

	"github.com/Bytonomics/cld-gateway/core"
)

// Request mirrors Rust CodexBackendRequest
type Request struct {
	AccessToken        core.Secret
	AccountID          string
	Model              string
	Instructions       string
	Input              []map[string]any
	Tools              []map[string]any
	ToolChoice         string
	ParallelToolCalls  bool
	Text               *map[string]any
	Reasoning          *map[string]any
	PreviousResponseID *string
	Stream             bool
	Include            []string
	ServiceTier        *string
	ClientMetadata     map[string]string
}

type Response struct {
	Status uint16
	Body   string
}

// UpstreamStatusError is implemented by any error type that carries the
// real HTTP status and response body a backend (e.g. OpenAI's Codex/
// Responses API) returned. core/domain/errors' classification function
// uses errors.As against this interface — never a concrete impl-layer
// type — to decide whether a failure originated upstream or inside the
// gateway itself, keeping core/domain/errors free of any import on
// core/impl/port/backend/codex (domain must not import impl).
type UpstreamStatusError interface {
	error
	UpstreamStatus() int
	UpstreamBody() string
}

// Event: Type from parsed "type"; Data = full raw JSON
type Event struct {
	Type string          `json:"type"`
	Data json.RawMessage `json:"-"`
}

// TerminalEvents from websocket_transport.rs:777
var TerminalEvents = []string{
	"response.completed",
	"response.failed",
	"response.cancelled",
	"error",
}
