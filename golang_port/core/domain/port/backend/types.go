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
