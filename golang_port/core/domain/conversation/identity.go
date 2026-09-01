package conversation

import (
	"github.com/Bytonomics/cld-gateway/core/domain/port/backend"
)

// Identity ports ConversationTransportIdentity (lib.rs:192-235): the key
// that scopes a turn to a conversation branch, request kind, and provider
// model/effort combination, for lease acquisition, WebSocket session
// pooling, and checkpoint association.
type Identity struct {
	ClaudeSessionID          string
	BranchID                 string
	Kind                     Kind
	ProviderModelFingerprint string
	ReasoningEffort          string
}

// Key ports ConversationTransportIdentity::key (lib.rs:217-226). The format
// string is load-bearing: it is used as the lease-registry key, the
// WebSocket session key, and the checkpoint-key prefix.
func (i Identity) Key() string {
	return "v1:" + i.ClaudeSessionID + ":" + i.BranchID + ":" + string(i.Kind) + ":" +
		i.ProviderModelFingerprint + ":" + i.ReasoningEffort
}

// SessionKey ports ConversationTransportIdentity::websocket_session_key
// (lib.rs:228-230).
func (i Identity) SessionKey() backend.SessionKey {
	return backend.SessionKey(i.Key())
}

// CheckpointKey ports ConversationTransportIdentity::checkpoint_key
// (lib.rs:232-234).
func (i Identity) CheckpointKey(responseID string) string {
	return i.Key() + ":" + responseID
}
