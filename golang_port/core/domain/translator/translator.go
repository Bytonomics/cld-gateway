// Package translator defines the BackendTranslator port and the shared
// policy helpers (tool-arg sanitization, response-gate cleanup) consumed by
// concrete per-backend translators. Port of the translation surface in
// crates/gateway-http-anthropic (translate.rs, tool_arg_policy.rs,
// claude_response_gate.rs).
package translator

import (
	"context"

	"github.com/Bytonomics/cld-gateway/core/domain/dto"
	"github.com/Bytonomics/cld-gateway/core/domain/port/backend"
)

// TranslateMeta carries per-request resolved metadata that a translator
// needs but that does not live on dto.MessagesRequest itself (resolved
// model, reasoning effort, service tier).
type TranslateMeta struct {
	Model           string
	ReasoningEffort string
	ServiceTier     *string
}

// BackendTranslator converts between the Anthropic-shaped request/response
// DTOs and a specific backend's wire shapes. One implementation per active
// backend; GenericBackendTranslator (a later wave) supplies the shared
// behavior that per-backend translators embed and override.
type BackendTranslator interface {
	TranslateRequest(ctx context.Context, in *dto.MessagesRequest, meta TranslateMeta) (*backend.Request, error)
	TranslateResponseEvent(ev backend.Event) ([]dto.SSEEvent, error)
	BuildUnaryResponse(events []backend.Event) (*dto.MessagesResponse, error)
	// GetToolCallKinds returns the tool-call kinds (by call ID) extracted
	// during the most recent BuildUnaryResponse or stream processing.
	// Returns a map from call_id to the canonical wire-format ToolCallKind string.
	GetToolCallKinds() map[string]string
}
