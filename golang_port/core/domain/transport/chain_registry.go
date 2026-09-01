package transport

import (
	"sync"

	"github.com/Bytonomics/cld-gateway/core/domain/conversation"
	"github.com/Bytonomics/cld-gateway/core/domain/port/backend"
)

// ChainRegistry ports the checkpoint_key -> WebSocketChainId association map
// (crates/gateway-http-anthropic/src/lib.rs:242-266, 2304-2306) —
// checkpoint_key = identity + response_id, see
// conversation.Identity.CheckpointKey. Selector consults this to enforce
// invariant #2: WS-delta reuse only when the live chain matches the chain
// stored here for this exact identity+response_id pair.
type ChainRegistry interface {
	Associate(identity conversation.Identity, responseID string, chain backend.ChainID)
	Lookup(identity conversation.Identity, responseID string) (backend.ChainID, bool)
}

// InMemoryChainRegistry is process-local only, matching Rust's current
// behavior. TODO(✱G1, open gap per GAPS.md/FILEMAP.md): an optional
// persisted variant into branch metadata is not implemented here.
type InMemoryChainRegistry struct {
	mu     sync.RWMutex
	chains map[string]backend.ChainID
	diag   DiagnosticSink
}

var _ ChainRegistry = (*InMemoryChainRegistry)(nil)

// NewInMemoryChainRegistry accepts a nil diag; diag is optional so tests and
// call sites without observability wiring can construct a registry freely.
func NewInMemoryChainRegistry(diag DiagnosticSink) *InMemoryChainRegistry {
	return &InMemoryChainRegistry{chains: make(map[string]backend.ChainID), diag: diag}
}

// Associate stores the checkpoint->chain link and, ported from
// OpenAiChainCheckpointStore::associate (lib.rs:238-268), emits the
// "checkpoint_associated" diagnostic other tooling may match on.
func (r *InMemoryChainRegistry) Associate(identity conversation.Identity, responseID string, chain backend.ChainID) {
	key := identity.CheckpointKey(responseID)
	r.mu.Lock()
	r.chains[key] = chain
	r.mu.Unlock()

	if r.diag == nil {
		return
	}
	_ = r.diag.Append(map[string]any{
		"event":                      "checkpoint_associated",
		"transport_identity":         identity.Key(),
		"claude_session_id":          identity.ClaudeSessionID,
		"branch_id":                  identity.BranchID,
		"provider_model_fingerprint": identity.ProviderModelFingerprint,
		"reasoning_effort":           identity.ReasoningEffort,
		"provider_response_id":       responseID,
		"websocket_chain_id":         chain.String(),
	})
}

func (r *InMemoryChainRegistry) Lookup(identity conversation.Identity, responseID string) (backend.ChainID, bool) {
	key := identity.CheckpointKey(responseID)
	r.mu.RLock()
	defer r.mu.RUnlock()
	chain, ok := r.chains[key]
	return chain, ok
}
