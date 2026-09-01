// Package conversation ports the request-kind classification and transport
// identity types used to route a turn to the correct conversation branch,
// persistence scope, and WebSocket session. Port of
// crates/gateway-http-anthropic/src/lib.rs:150-235
// (ConversationRequestKind, ConversationTransportIdentity).
package conversation

// Kind mirrors ConversationRequestKind (lib.rs:150-159), serialized with its
// as_key() string values (lib.rs:162-172) since those strings are load-bearing
// in ConversationTransportIdentity.Key().
type Kind string

const (
	VisibleMain          Kind = "visible_main"
	SubagentOffshoot     Kind = "subagent_offshoot"
	PermissionClassifier Kind = "permission_classifier"
	HookEvaluator        Kind = "hook_evaluator"
	LocalControl         Kind = "local_control"
	StatusOrAuxiliary    Kind = "status_or_auxiliary"
	UnknownOffshoot      Kind = "unknown_offshoot"
)

// PersistenceReason ports ConversationRequestKind::persistence_reason
// (lib.rs:174-184). PermissionClassifier is the one case where the
// persistence reason string differs from the key string.
func (k Kind) PersistenceReason() string {
	if k == PermissionClassifier {
		return "permission_or_classifier_transcript"
	}
	return string(k)
}

// IsVisibleMain ports ConversationRequestKind::is_visible_main (lib.rs:186-188).
func (k Kind) IsVisibleMain() bool {
	return k == VisibleMain
}
