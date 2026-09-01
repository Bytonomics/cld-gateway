package state

import "context"

// StoredToolCall is a Go-idiom extension (not a deviation from the pinned
// type list) of Rust's tool_calls.rs StoredToolCall{tool_name, tool_kind}:
// FILEMAP's RecordToolCall takes a single StoredToolCall param, so the
// call_id/request_id/created_at that Rust passes as separate function
// arguments are folded into this struct instead.
type StoredToolCall struct {
	CallID               string
	ToolName             string
	ToolKind             string
	RequestID            *string
	CreatedAtUnixSeconds int64
}

// ToolCallRepo is a port of ToolCallStore (crates/gateway-state/src/tool_calls.rs).
// Interface only; the GORM/sqlite implementation lands in a later wave.
type ToolCallRepo interface {
	EnsureSchema(ctx context.Context) error
	RecordToolCall(ctx context.Context, call StoredToolCall) error
	ToolCallExists(ctx context.Context, callID string) (bool, error)
	GetToolCall(ctx context.Context, callID string) (*StoredToolCall, error)
}
