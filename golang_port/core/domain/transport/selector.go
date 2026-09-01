package transport

import (
	"context"

	"github.com/Bytonomics/cld-gateway/core/domain/conversation"
	"github.com/Bytonomics/cld-gateway/core/domain/port/backend"
)

// DiagnosticSink is the observability/transport_diag.go writer's shape
// (*observability.TransportDiagLog satisfies it) — the transport package
// takes it as an interface rather than importing observability directly, to
// keep this domain package free of an infra import while still routing
// every diagnostic through the one JSONL sink (no second writer).
type DiagnosticSink interface {
	Append(record any) error
}

// Selector decides whether a request may reuse an incremental (WebSocket
// delta) transport, per behavioral invariant #2 (ARCHITECTURE_v2.md):
// previous_response_id / WS-delta reuse is allowed ONLY when the live WS
// chain for this session matches the chain ChainRegistry has stored against
// checkpoint_key = identity + response_id. A live chain existing is never,
// by itself, grounds for reuse — ports
// websocket_chain_decision_for_branch (lib.rs:2257-2337).
type Selector interface {
	Select(ctx context.Context, plan Plan) (Decision, error)
}

// Plan is the input to a transport selection decision. Callers resolve the
// checkpoint candidate and live-chain state before calling Select; Selector
// applies the reuse rule, it does not look either up itself (registry
// lookup excepted, which is the rule).
type Plan struct {
	Identity  conversation.Identity
	RequestID string

	HasCheckpoint      bool
	PreviousResponseID string

	SessionKey   backend.SessionKey
	HasLiveChain bool
	LiveChain    backend.ChainID
}

type Decision struct {
	UseWS  bool
	Chain  backend.ChainID
	Reason string
}

// Reason values, ported verbatim from websocket_chain_decision_for_branch
// (crates/gateway-http-anthropic/src/lib.rs:2257-2337) — other tooling may
// match on these exact strings.
const (
	ReasonMissingCheckpoint                     = "missing_checkpoint"
	ReasonMissingLiveWebSocketChain             = "missing_live_websocket_chain"
	ReasonMissingCheckpointWebSocketAssociation = "missing_checkpoint_websocket_chain_association"
	ReasonWebSocketChainMatch                   = "websocket_chain_match"
	ReasonWebSocketChainMismatch                = "websocket_chain_mismatch"
)

type selector struct {
	registry ChainRegistry
	diag     DiagnosticSink
}

var _ Selector = (*selector)(nil)

func NewSelector(registry ChainRegistry, diag DiagnosticSink) Selector {
	return &selector{registry: registry, diag: diag}
}

func (s *selector) Select(ctx context.Context, plan Plan) (Decision, error) {
	_ = ctx
	decision, checkpointChain, checkpointChainKnown := s.decide(plan)
	s.emit(plan, decision, checkpointChain, checkpointChainKnown)
	return decision, nil
}

func (s *selector) decide(plan Plan) (Decision, backend.ChainID, bool) {
	if !plan.HasCheckpoint {
		return Decision{UseWS: false, Reason: ReasonMissingCheckpoint}, "", false
	}
	if !plan.HasLiveChain {
		return Decision{UseWS: false, Reason: ReasonMissingLiveWebSocketChain}, "", false
	}

	checkpointChain, ok := s.registry.Lookup(plan.Identity, plan.PreviousResponseID)
	if !ok {
		return Decision{UseWS: false, Reason: ReasonMissingCheckpointWebSocketAssociation}, "", false
	}

	if checkpointChain != plan.LiveChain {
		return Decision{UseWS: false, Reason: ReasonWebSocketChainMismatch}, checkpointChain, true
	}

	return Decision{UseWS: true, Chain: checkpointChain, Reason: ReasonWebSocketChainMatch}, checkpointChain, true
}

func (s *selector) emit(plan Plan, decision Decision, checkpointChain backend.ChainID, checkpointChainKnown bool) {
	if s.diag == nil {
		return
	}
	fields := map[string]any{
		"event":                "websocket_chain_decision",
		"request_id":           plan.RequestID,
		"transport_identity":   plan.Identity.Key(),
		"previous_response_id": plan.PreviousResponseID,
		"reason":               decision.Reason,
		"use_websocket":        decision.UseWS,
	}
	if plan.HasLiveChain {
		fields["live_websocket_chain_id"] = plan.LiveChain.String()
	} else {
		fields["live_websocket_chain_id"] = nil
	}
	if checkpointChainKnown {
		fields["checkpoint_websocket_chain_id"] = checkpointChain.String()
	} else {
		fields["checkpoint_websocket_chain_id"] = nil
	}
	_ = s.diag.Append(fields)
}
