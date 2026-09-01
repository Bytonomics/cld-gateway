package conversation

import (
	"encoding/json"
	"fmt"

	"github.com/Bytonomics/cld-gateway/core/domain/port/state"
)

// This file defines the on-disk JSON shapes for conversation state. The
// domain types in core/domain/port/state/types.go carry no `json` tags, so
// marshaling them directly would emit Go field names (PascalCase) instead
// of the snake_case keys the Rust build already wrote to disk. These wire
// structs (and the To/From converters below) are the compatibility shim:
// every read/write of session.json, branch.json, ledger.jsonl, and sparse
// checkpoint files goes through them.

type branchCheckpointRefWire struct {
	BranchID     string `json:"branch_id"`
	CheckpointID string `json:"checkpoint_id"`
}

func toBranchCheckpointRefWire(r *state.BranchCheckpointRef) *branchCheckpointRefWire {
	if r == nil {
		return nil
	}
	return &branchCheckpointRefWire{BranchID: r.BranchID, CheckpointID: r.CheckpointID}
}

func fromBranchCheckpointRefWire(w *branchCheckpointRefWire) *state.BranchCheckpointRef {
	if w == nil {
		return nil
	}
	return &state.BranchCheckpointRef{BranchID: w.BranchID, CheckpointID: w.CheckpointID}
}

type fingerprintSetWire struct {
	RecentMessageTailHash *string `json:"recent_message_tail_hash"`
	LastUserMessageHash   *string `json:"last_user_message_hash"`
	CompactionSummaryHash *string `json:"compaction_summary_hash"`
	BranchStateHash       *string `json:"branch_state_hash"`
}

func fingerprintsToWire(f state.BranchFingerprintSet) fingerprintSetWire {
	return fingerprintSetWire{
		RecentMessageTailHash: f.RecentMessageTailHash,
		LastUserMessageHash:   f.LastUserMessageHash,
		CompactionSummaryHash: f.CompactionSummaryHash,
		BranchStateHash:       f.BranchStateHash,
	}
}

func fingerprintsFromWire(w fingerprintSetWire) state.BranchFingerprintSet {
	return state.BranchFingerprintSet{
		RecentMessageTailHash: w.RecentMessageTailHash,
		LastUserMessageHash:   w.LastUserMessageHash,
		CompactionSummaryHash: w.CompactionSummaryHash,
		BranchStateHash:       w.BranchStateHash,
	}
}

type openAICheckpointWire struct {
	ResponseID                      string  `json:"response_id"`
	PreviousResponseID              *string `json:"previous_response_id"`
	ProviderModelFingerprint        string  `json:"provider_model_fingerprint"`
	RequestCompatibilityFingerprint *string `json:"request_compatibility_fingerprint"`
	ProviderInputTokens             *int64  `json:"provider_input_tokens"`
}

func toOpenAICheckpointWire(c *state.OpenAiCheckpoint) *openAICheckpointWire {
	if c == nil {
		return nil
	}
	return &openAICheckpointWire{
		ResponseID:                      c.ResponseID,
		PreviousResponseID:              c.PreviousResponseID,
		ProviderModelFingerprint:        c.ProviderModelFingerprint,
		RequestCompatibilityFingerprint: c.RequestCompatibilityFingerprint,
		ProviderInputTokens:             c.ProviderInputTokens,
	}
}

func fromOpenAICheckpointWire(w *openAICheckpointWire) *state.OpenAiCheckpoint {
	if w == nil {
		return nil
	}
	return &state.OpenAiCheckpoint{
		ResponseID:                      w.ResponseID,
		PreviousResponseID:              w.PreviousResponseID,
		ProviderModelFingerprint:        w.ProviderModelFingerprint,
		RequestCompatibilityFingerprint: w.RequestCompatibilityFingerprint,
		ProviderInputTokens:             w.ProviderInputTokens,
	}
}

type turnOpenAICheckpointWire struct {
	SchemaVersion                   uint32  `json:"schema_version"`
	TurnID                          string  `json:"turn_id"`
	CanonicalMessageCount           uint64  `json:"canonical_message_count"`
	CanonicalPrefixHash             string  `json:"canonical_prefix_hash"`
	ResponseID                      string  `json:"response_id"`
	PreviousResponseID              *string `json:"previous_response_id"`
	ProviderModelFingerprint        string  `json:"provider_model_fingerprint"`
	RequestCompatibilityFingerprint *string `json:"request_compatibility_fingerprint"`
	ProviderInputTokens             *int64  `json:"provider_input_tokens"`
	CreatedAtUnixSeconds            int64   `json:"created_at_unix_seconds"`
}

func toTurnCheckpointWireSlice(cs []state.TurnOpenAiCheckpoint) []turnOpenAICheckpointWire {
	out := make([]turnOpenAICheckpointWire, len(cs))
	for i, c := range cs {
		out[i] = turnOpenAICheckpointWire{
			SchemaVersion:                   c.SchemaVersion,
			TurnID:                          c.TurnID,
			CanonicalMessageCount:           c.CanonicalMessageCount,
			CanonicalPrefixHash:             c.CanonicalPrefixHash,
			ResponseID:                      c.ResponseID,
			PreviousResponseID:              c.PreviousResponseID,
			ProviderModelFingerprint:        c.ProviderModelFingerprint,
			RequestCompatibilityFingerprint: c.RequestCompatibilityFingerprint,
			ProviderInputTokens:             c.ProviderInputTokens,
			CreatedAtUnixSeconds:            c.CreatedAtUnixSeconds,
		}
	}
	return out
}

func fromTurnCheckpointWireSlice(ws []turnOpenAICheckpointWire) []state.TurnOpenAiCheckpoint {
	out := make([]state.TurnOpenAiCheckpoint, len(ws))
	for i, w := range ws {
		out[i] = state.TurnOpenAiCheckpoint{
			SchemaVersion:                   w.SchemaVersion,
			TurnID:                          w.TurnID,
			CanonicalMessageCount:           w.CanonicalMessageCount,
			CanonicalPrefixHash:             w.CanonicalPrefixHash,
			ResponseID:                      w.ResponseID,
			PreviousResponseID:              w.PreviousResponseID,
			ProviderModelFingerprint:        w.ProviderModelFingerprint,
			RequestCompatibilityFingerprint: w.RequestCompatibilityFingerprint,
			ProviderInputTokens:             w.ProviderInputTokens,
			CreatedAtUnixSeconds:            w.CreatedAtUnixSeconds,
		}
	}
	return out
}

type offshootOpenAICheckpointWire struct {
	SchemaVersion                   uint32  `json:"schema_version"`
	OffshootIdentity                string  `json:"offshoot_identity"`
	ResponseID                      string  `json:"response_id"`
	PreviousResponseID              *string `json:"previous_response_id"`
	ProviderModelFingerprint        string  `json:"provider_model_fingerprint"`
	RequestCompatibilityFingerprint *string `json:"request_compatibility_fingerprint"`
	ProviderInputTokens             *int64  `json:"provider_input_tokens"`
	CreatedAtUnixSeconds            int64   `json:"created_at_unix_seconds"`
}

func toOffshootCheckpointWireSlice(cs []state.OffshootOpenAiCheckpoint) []offshootOpenAICheckpointWire {
	out := make([]offshootOpenAICheckpointWire, len(cs))
	for i, c := range cs {
		out[i] = offshootOpenAICheckpointWire{
			SchemaVersion:                   c.SchemaVersion,
			OffshootIdentity:                c.OffshootIdentity,
			ResponseID:                      c.ResponseID,
			PreviousResponseID:              c.PreviousResponseID,
			ProviderModelFingerprint:        c.ProviderModelFingerprint,
			RequestCompatibilityFingerprint: c.RequestCompatibilityFingerprint,
			ProviderInputTokens:             c.ProviderInputTokens,
			CreatedAtUnixSeconds:            c.CreatedAtUnixSeconds,
		}
	}
	return out
}

func fromOffshootCheckpointWireSlice(ws []offshootOpenAICheckpointWire) []state.OffshootOpenAiCheckpoint {
	out := make([]state.OffshootOpenAiCheckpoint, len(ws))
	for i, w := range ws {
		out[i] = state.OffshootOpenAiCheckpoint{
			SchemaVersion:                   w.SchemaVersion,
			OffshootIdentity:                w.OffshootIdentity,
			ResponseID:                      w.ResponseID,
			PreviousResponseID:              w.PreviousResponseID,
			ProviderModelFingerprint:        w.ProviderModelFingerprint,
			RequestCompatibilityFingerprint: w.RequestCompatibilityFingerprint,
			ProviderInputTokens:             w.ProviderInputTokens,
			CreatedAtUnixSeconds:            w.CreatedAtUnixSeconds,
		}
	}
	return out
}

type branchWire struct {
	SchemaVersion             uint32                         `json:"schema_version"`
	BranchID                  string                         `json:"branch_id"`
	ParentBranchID            *string                        `json:"parent_branch_id"`
	ForkAncestorCheckpoint    *branchCheckpointRefWire       `json:"fork_ancestor_checkpoint"`
	CurrentCheckpointID       *string                        `json:"current_checkpoint_id"`
	ActiveCanonicalMessages   any                            `json:"active_canonical_messages"`
	Fingerprints              fingerprintSetWire             `json:"fingerprints"`
	OpenAiCheckpoint          *openAICheckpointWire          `json:"openai_checkpoint"`
	TurnOpenAiCheckpoints     []turnOpenAICheckpointWire     `json:"turn_openai_checkpoints"`
	OffshootOpenAiCheckpoints []offshootOpenAICheckpointWire `json:"offshoot_openai_checkpoints"`
	CompactionResetPending    bool                           `json:"compaction_reset_pending"`
	LastMainTurnID            *string                        `json:"last_main_turn_id"`
	CreatedAtUnixSeconds      int64                          `json:"created_at_unix_seconds"`
	UpdatedAtUnixSeconds      int64                          `json:"updated_at_unix_seconds"`
}

func branchToWire(b state.BranchMetadata) branchWire {
	return branchWire{
		SchemaVersion:             b.SchemaVersion,
		BranchID:                  b.BranchID,
		ParentBranchID:            b.ParentBranchID,
		ForkAncestorCheckpoint:    toBranchCheckpointRefWire(b.ForkAncestorCheckpoint),
		CurrentCheckpointID:       b.CurrentCheckpointID,
		ActiveCanonicalMessages:   b.ActiveCanonicalMessages,
		Fingerprints:              fingerprintsToWire(b.Fingerprints),
		OpenAiCheckpoint:          toOpenAICheckpointWire(b.OpenAiCheckpoint),
		TurnOpenAiCheckpoints:     toTurnCheckpointWireSlice(b.TurnOpenAiCheckpoints),
		OffshootOpenAiCheckpoints: toOffshootCheckpointWireSlice(b.OffshootOpenAiCheckpoints),
		CompactionResetPending:    b.CompactionResetPending,
		LastMainTurnID:            b.LastMainTurnID,
		CreatedAtUnixSeconds:      b.CreatedAtUnixSeconds,
		UpdatedAtUnixSeconds:      b.UpdatedAtUnixSeconds,
	}
}

func branchFromWire(w branchWire) state.BranchMetadata {
	return state.BranchMetadata{
		SchemaVersion:             w.SchemaVersion,
		BranchID:                  w.BranchID,
		ParentBranchID:            w.ParentBranchID,
		ForkAncestorCheckpoint:    fromBranchCheckpointRefWire(w.ForkAncestorCheckpoint),
		CurrentCheckpointID:       w.CurrentCheckpointID,
		ActiveCanonicalMessages:   w.ActiveCanonicalMessages,
		Fingerprints:              fingerprintsFromWire(w.Fingerprints),
		OpenAiCheckpoint:          fromOpenAICheckpointWire(w.OpenAiCheckpoint),
		TurnOpenAiCheckpoints:     fromTurnCheckpointWireSlice(w.TurnOpenAiCheckpoints),
		OffshootOpenAiCheckpoints: fromOffshootCheckpointWireSlice(w.OffshootOpenAiCheckpoints),
		CompactionResetPending:    w.CompactionResetPending,
		LastMainTurnID:            w.LastMainTurnID,
		CreatedAtUnixSeconds:      w.CreatedAtUnixSeconds,
		UpdatedAtUnixSeconds:      w.UpdatedAtUnixSeconds,
	}
}

type sessionWire struct {
	SchemaVersion        uint32   `json:"schema_version"`
	ClaudeSessionID      string   `json:"claude_session_id"`
	CreatedAtUnixSeconds int64    `json:"created_at_unix_seconds"`
	UpdatedAtUnixSeconds int64    `json:"updated_at_unix_seconds"`
	BranchIDs            []string `json:"branch_ids"`
}

func sessionToWire(s state.ClaudeSessionMetadata) sessionWire {
	branchIDs := s.BranchIDs
	if branchIDs == nil {
		branchIDs = []string{}
	}
	return sessionWire{
		SchemaVersion:        s.SchemaVersion,
		ClaudeSessionID:      s.ClaudeSessionID,
		CreatedAtUnixSeconds: s.CreatedAtUnixSeconds,
		UpdatedAtUnixSeconds: s.UpdatedAtUnixSeconds,
		BranchIDs:            branchIDs,
	}
}

func sessionFromWire(w sessionWire) state.ClaudeSessionMetadata {
	return state.ClaudeSessionMetadata{
		SchemaVersion:        w.SchemaVersion,
		ClaudeSessionID:      w.ClaudeSessionID,
		CreatedAtUnixSeconds: w.CreatedAtUnixSeconds,
		UpdatedAtUnixSeconds: w.UpdatedAtUnixSeconds,
		BranchIDs:            w.BranchIDs,
	}
}

type sparseCheckpointWire struct {
	SchemaVersion           uint32                   `json:"schema_version"`
	CheckpointKind          string                   `json:"checkpoint_kind"`
	BranchID                string                   `json:"branch_id"`
	CurrentCheckpointID     *string                  `json:"current_checkpoint_id"`
	ParentBranchID          *string                  `json:"parent_branch_id"`
	ForkAncestorCheckpoint  *branchCheckpointRefWire `json:"fork_ancestor_checkpoint"`
	ActiveCanonicalMessages any                      `json:"active_canonical_messages"`
	Fingerprints            fingerprintSetWire       `json:"fingerprints"`
	CompactionResetPending  bool                     `json:"compaction_reset_pending"`
	LastMainTurnID          *string                  `json:"last_main_turn_id"`
	CreatedAtUnixSeconds    int64                    `json:"created_at_unix_seconds"`
}

// --- ledger events (serde internally-tagged enum parity: one flat JSON
// object per line with an "event_type" discriminant) ---

type ledgerEventBranchCreatedWire struct {
	EventType              string                   `json:"event_type"`
	BranchID               string                   `json:"branch_id"`
	ParentBranchID         *string                  `json:"parent_branch_id"`
	ForkAncestorCheckpoint *branchCheckpointRefWire `json:"fork_ancestor_checkpoint"`
	CreatedAtUnixSeconds   int64                    `json:"created_at_unix_seconds"`
}

type ledgerEventSnapshotReconciledWire struct {
	EventType            string  `json:"event_type"`
	SnapshotHash         *string `json:"snapshot_hash"`
	CreatedAtUnixSeconds int64   `json:"created_at_unix_seconds"`
}

type ledgerEventMainTurnCommittedWire struct {
	EventType            string  `json:"event_type"`
	TurnID               string  `json:"turn_id"`
	ProviderResponseID   *string `json:"provider_response_id"`
	RequestFingerprint   *string `json:"request_fingerprint"`
	ProviderOutputItems  []any   `json:"provider_output_items"`
	CreatedAtUnixSeconds int64   `json:"created_at_unix_seconds"`
}

type ledgerEventSideTurnObservedWire struct {
	EventType            string  `json:"event_type"`
	TurnID               string  `json:"turn_id"`
	RequestFingerprint   *string `json:"request_fingerprint"`
	ProviderOutputItems  []any   `json:"provider_output_items"`
	CreatedAtUnixSeconds int64   `json:"created_at_unix_seconds"`
}

type ledgerEventCompactionAppliedWire struct {
	EventType            string  `json:"event_type"`
	SummaryHash          *string `json:"summary_hash"`
	CreatedAtUnixSeconds int64   `json:"created_at_unix_seconds"`
}

func marshalLedgerEvent(ev state.CanonicalLedgerEvent) ([]byte, error) {
	switch ev.EventType {
	case state.CanonicalLedgerEventTypeBranchCreated:
		p := ev.BranchCreated
		if p == nil {
			return nil, fmt.Errorf("ledger event %q missing branch_created payload", ev.EventType)
		}
		return json.Marshal(ledgerEventBranchCreatedWire{
			EventType:              string(ev.EventType),
			BranchID:               p.BranchID,
			ParentBranchID:         p.ParentBranchID,
			ForkAncestorCheckpoint: toBranchCheckpointRefWire(p.ForkAncestorCheckpoint),
			CreatedAtUnixSeconds:   p.CreatedAtUnixSeconds,
		})
	case state.CanonicalLedgerEventTypeInboundCanonicalSnapshotReconciled:
		p := ev.InboundCanonicalSnapshotReconciled
		if p == nil {
			return nil, fmt.Errorf("ledger event %q missing inbound_canonical_snapshot_reconciled payload", ev.EventType)
		}
		return json.Marshal(ledgerEventSnapshotReconciledWire{
			EventType:            string(ev.EventType),
			SnapshotHash:         p.SnapshotHash,
			CreatedAtUnixSeconds: p.CreatedAtUnixSeconds,
		})
	case state.CanonicalLedgerEventTypeMainTurnCommitted:
		p := ev.MainTurnCommitted
		if p == nil {
			return nil, fmt.Errorf("ledger event %q missing main_turn_committed payload", ev.EventType)
		}
		return json.Marshal(ledgerEventMainTurnCommittedWire{
			EventType:            string(ev.EventType),
			TurnID:               p.TurnID,
			ProviderResponseID:   p.ProviderResponseID,
			RequestFingerprint:   p.RequestFingerprint,
			ProviderOutputItems:  nonNilAnySlice(p.ProviderOutputItems),
			CreatedAtUnixSeconds: p.CreatedAtUnixSeconds,
		})
	case state.CanonicalLedgerEventTypeSideTurnObserved:
		p := ev.SideTurnObserved
		if p == nil {
			return nil, fmt.Errorf("ledger event %q missing side_turn_observed payload", ev.EventType)
		}
		return json.Marshal(ledgerEventSideTurnObservedWire{
			EventType:            string(ev.EventType),
			TurnID:               p.TurnID,
			RequestFingerprint:   p.RequestFingerprint,
			ProviderOutputItems:  nonNilAnySlice(p.ProviderOutputItems),
			CreatedAtUnixSeconds: p.CreatedAtUnixSeconds,
		})
	case state.CanonicalLedgerEventTypeCompactionApplied:
		p := ev.CompactionApplied
		if p == nil {
			return nil, fmt.Errorf("ledger event %q missing compaction_applied payload", ev.EventType)
		}
		return json.Marshal(ledgerEventCompactionAppliedWire{
			EventType:            string(ev.EventType),
			SummaryHash:          p.SummaryHash,
			CreatedAtUnixSeconds: p.CreatedAtUnixSeconds,
		})
	default:
		return nil, fmt.Errorf("unknown ledger event_type %q", ev.EventType)
	}
}

func unmarshalLedgerEvent(data []byte) (state.CanonicalLedgerEvent, error) {
	var head struct {
		EventType string `json:"event_type"`
	}
	if err := json.Unmarshal(data, &head); err != nil {
		return state.CanonicalLedgerEvent{}, err
	}

	switch state.CanonicalLedgerEventType(head.EventType) {
	case state.CanonicalLedgerEventTypeBranchCreated:
		var w ledgerEventBranchCreatedWire
		if err := json.Unmarshal(data, &w); err != nil {
			return state.CanonicalLedgerEvent{}, err
		}
		return state.CanonicalLedgerEvent{
			EventType: state.CanonicalLedgerEventTypeBranchCreated,
			BranchCreated: &state.LedgerEventBranchCreated{
				BranchID:               w.BranchID,
				ParentBranchID:         w.ParentBranchID,
				ForkAncestorCheckpoint: fromBranchCheckpointRefWire(w.ForkAncestorCheckpoint),
				CreatedAtUnixSeconds:   w.CreatedAtUnixSeconds,
			},
		}, nil
	case state.CanonicalLedgerEventTypeInboundCanonicalSnapshotReconciled:
		var w ledgerEventSnapshotReconciledWire
		if err := json.Unmarshal(data, &w); err != nil {
			return state.CanonicalLedgerEvent{}, err
		}
		return state.CanonicalLedgerEvent{
			EventType: state.CanonicalLedgerEventTypeInboundCanonicalSnapshotReconciled,
			InboundCanonicalSnapshotReconciled: &state.LedgerEventInboundCanonicalSnapshotReconciled{
				SnapshotHash:         w.SnapshotHash,
				CreatedAtUnixSeconds: w.CreatedAtUnixSeconds,
			},
		}, nil
	case state.CanonicalLedgerEventTypeMainTurnCommitted:
		var w ledgerEventMainTurnCommittedWire
		if err := json.Unmarshal(data, &w); err != nil {
			return state.CanonicalLedgerEvent{}, err
		}
		return state.CanonicalLedgerEvent{
			EventType: state.CanonicalLedgerEventTypeMainTurnCommitted,
			MainTurnCommitted: &state.LedgerEventMainTurnCommitted{
				TurnID:               w.TurnID,
				ProviderResponseID:   w.ProviderResponseID,
				RequestFingerprint:   w.RequestFingerprint,
				ProviderOutputItems:  w.ProviderOutputItems,
				CreatedAtUnixSeconds: w.CreatedAtUnixSeconds,
			},
		}, nil
	case state.CanonicalLedgerEventTypeSideTurnObserved:
		var w ledgerEventSideTurnObservedWire
		if err := json.Unmarshal(data, &w); err != nil {
			return state.CanonicalLedgerEvent{}, err
		}
		return state.CanonicalLedgerEvent{
			EventType: state.CanonicalLedgerEventTypeSideTurnObserved,
			SideTurnObserved: &state.LedgerEventSideTurnObserved{
				TurnID:               w.TurnID,
				RequestFingerprint:   w.RequestFingerprint,
				ProviderOutputItems:  w.ProviderOutputItems,
				CreatedAtUnixSeconds: w.CreatedAtUnixSeconds,
			},
		}, nil
	case state.CanonicalLedgerEventTypeCompactionApplied:
		var w ledgerEventCompactionAppliedWire
		if err := json.Unmarshal(data, &w); err != nil {
			return state.CanonicalLedgerEvent{}, err
		}
		return state.CanonicalLedgerEvent{
			EventType: state.CanonicalLedgerEventTypeCompactionApplied,
			CompactionApplied: &state.LedgerEventCompactionApplied{
				SummaryHash:          w.SummaryHash,
				CreatedAtUnixSeconds: w.CreatedAtUnixSeconds,
			},
		}, nil
	default:
		return state.CanonicalLedgerEvent{}, fmt.Errorf("unknown ledger event_type %q", head.EventType)
	}
}
