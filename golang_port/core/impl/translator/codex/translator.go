// Package codex provides the Codex-backend BackendTranslator. It embeds
// translator.GenericBackendTranslator by pointer and relies entirely on the
// embedded methods (request shaping, tool-arg policy, and the
// sse_bridge.go response-event/unary-response state machine, which is
// itself Codex wire-format specific) to satisfy translator.BackendTranslator.
package codex

import (
	"github.com/Bytonomics/cld-gateway/config"
	"github.com/Bytonomics/cld-gateway/core/domain/port/state"
	"github.com/Bytonomics/cld-gateway/core/domain/translator"
)

// OpenAITranslator is the Codex/OpenAI Responses-backend BackendTranslator.
type OpenAITranslator struct {
	*translator.GenericBackendTranslator
}

var _ translator.BackendTranslator = (*OpenAITranslator)(nil)

// New constructs an OpenAITranslator for a single request. toolCallKinds
// resolves the backend tool-call kind (function/custom/tool_search/
// local_shell) for a call_id; requestID, structuredOutputSchema, and
// contextManagementValue are the per-request context sse_bridge.rs's
// map_backend_event/build_unary_messages_response receive as explicit
// parameters in Rust.
func New(
	claudeCodeConfig config.ClaudeCodeWorkflowConfig,
	toolCallKinds translator.ToolCallKindLookup,
	toolCalls state.ToolCallRepo,
	requestID *string,
	structuredOutputSchema any,
	contextManagementValue any,
	responseModel string,
	clock state.Clock,
) *OpenAITranslator {
	return &OpenAITranslator{
		GenericBackendTranslator: &translator.GenericBackendTranslator{
			ClaudeCodeConfig:       claudeCodeConfig,
			ToolCallKinds:          toolCallKinds,
			ToolCalls:              toolCalls,
			RequestID:              requestID,
			StructuredOutputSchema: structuredOutputSchema,
			ContextManagementValue: contextManagementValue,
			ResponseModel:          responseModel,
			Clock:                  clock,
		},
	}
}
