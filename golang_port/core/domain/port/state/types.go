package state

// Port of crates/gateway-state/src/conversation.rs type/enum definitions.

type ClaudeSessionMetadata struct {
	SchemaVersion        uint32
	ClaudeSessionID      string
	CreatedAtUnixSeconds int64
	UpdatedAtUnixSeconds int64
	BranchIDs            []string
}

type BranchFingerprintSet struct {
	RecentMessageTailHash *string
	LastUserMessageHash   *string
	CompactionSummaryHash *string
	BranchStateHash       *string
}

type OpenAiCheckpoint struct {
	ResponseID                      string
	PreviousResponseID              *string
	ProviderModelFingerprint        string
	RequestCompatibilityFingerprint *string
	ProviderInputTokens             *int64
}

type TurnOpenAiCheckpoint struct {
	SchemaVersion                   uint32
	TurnID                          string
	CanonicalMessageCount           uint64
	CanonicalPrefixHash             string
	ResponseID                      string
	PreviousResponseID              *string
	ProviderModelFingerprint        string
	RequestCompatibilityFingerprint *string
	ProviderInputTokens             *int64
	CreatedAtUnixSeconds            int64
}

type OffshootOpenAiCheckpoint struct {
	SchemaVersion                   uint32
	OffshootIdentity                string
	ResponseID                      string
	PreviousResponseID              *string
	ProviderModelFingerprint        string
	RequestCompatibilityFingerprint *string
	ProviderInputTokens             *int64
	CreatedAtUnixSeconds            int64
}

type BranchCheckpointRef struct {
	BranchID     string
	CheckpointID string
}

type BranchMetadata struct {
	SchemaVersion             uint32
	BranchID                  string
	ParentBranchID            *string
	ForkAncestorCheckpoint    *BranchCheckpointRef
	CurrentCheckpointID       *string
	ActiveCanonicalMessages   any
	Fingerprints              BranchFingerprintSet
	OpenAiCheckpoint          *OpenAiCheckpoint
	TurnOpenAiCheckpoints     []TurnOpenAiCheckpoint
	OffshootOpenAiCheckpoints []OffshootOpenAiCheckpoint
	CompactionResetPending    bool
	LastMainTurnID            *string
	CreatedAtUnixSeconds      int64
	UpdatedAtUnixSeconds      int64
}

type BranchCreateParams struct {
	ParentBranchID          *string
	ForkAncestorCheckpoint  *BranchCheckpointRef
	ActiveCanonicalMessages any
	Fingerprints            BranchFingerprintSet
}

// ConversationTurnScope mirrors the Rust enum, serde(rename_all = "snake_case").
type ConversationTurnScope string

const (
	ConversationTurnScopeMain ConversationTurnScope = "main"
	ConversationTurnScopeSide ConversationTurnScope = "side"
)

type BranchSelectionInput struct {
	ActiveCanonicalMessages any
	Fingerprints            BranchFingerprintSet
	TurnScope               ConversationTurnScope
}

// BranchSelectionAction mirrors the Rust enum, serde(rename_all = "snake_case").
type BranchSelectionAction string

const (
	BranchSelectionActionCreatedInitial     BranchSelectionAction = "created_initial"
	BranchSelectionActionContinuedExisting  BranchSelectionAction = "continued_existing"
	BranchSelectionActionForkedFromAncestor BranchSelectionAction = "forked_from_ancestor"
	BranchSelectionActionCreatedAmbiguous   BranchSelectionAction = "created_ambiguous"
	BranchSelectionActionCreatedUnmatched   BranchSelectionAction = "created_unmatched"
)

type BranchSelectionResult struct {
	Branch                BranchMetadata
	Action                BranchSelectionAction
	MatchedExistingBranch *BranchMetadata
}

type CommitTurnParams struct {
	TurnScope                       ConversationTurnScope
	TurnID                          string
	Fingerprints                    BranchFingerprintSet
	ActiveCanonicalMessages         any
	ProviderResponseID              *string
	PreviousResponseID              *string
	ProviderModelFingerprint        *string
	RequestCompatibilityFingerprint *string
	ProviderInputTokens             *int64
	CanonicalMessageCount           *uint64
	CanonicalPrefixHash             *string
	ProviderOutputItems             []any
}

type CommitOffshootCheckpointParams struct {
	OffshootIdentity                string
	ProviderResponseID              string
	PreviousResponseID              *string
	ProviderModelFingerprint        string
	RequestCompatibilityFingerprint *string
	ProviderInputTokens             *int64
}

type ReconcileSnapshotParams struct {
	Messages     any
	Fingerprints BranchFingerprintSet
}

// SparseCheckpointKind mirrors the Rust enum, serde(rename_all = "snake_case").
type SparseCheckpointKind string

const (
	SparseCheckpointKindBranchCreated     SparseCheckpointKind = "branch_created"
	SparseCheckpointKindBranchForkCreated SparseCheckpointKind = "branch_fork_created"
	SparseCheckpointKindCompactionApplied SparseCheckpointKind = "compaction_applied"
)

type SparseCheckpoint struct {
	SchemaVersion           uint32
	CheckpointKind          SparseCheckpointKind
	BranchID                string
	CurrentCheckpointID     *string
	ParentBranchID          *string
	ForkAncestorCheckpoint  *BranchCheckpointRef
	ActiveCanonicalMessages any
	Fingerprints            BranchFingerprintSet
	CompactionResetPending  bool
	LastMainTurnID          *string
	CreatedAtUnixSeconds    int64
}

// CanonicalLedgerEvent mirrors the Rust tagged-union enum
// (serde(tag = "event_type", rename_all = "snake_case")) with 5 variants.
//
// Go-idiom decision (not a deviation from the pinned type list): represented
// as a struct with an EventType discriminant plus one pointer field per
// variant payload, rather than an interface + 5 concrete types. This keeps
// JSON (de)serialization straightforward with a single tag field, mirroring
// the Rust serde adjacently/internally tagged shape.
type CanonicalLedgerEventType string

const (
	CanonicalLedgerEventTypeBranchCreated                      CanonicalLedgerEventType = "branch_created"
	CanonicalLedgerEventTypeInboundCanonicalSnapshotReconciled CanonicalLedgerEventType = "inbound_canonical_snapshot_reconciled"
	CanonicalLedgerEventTypeMainTurnCommitted                  CanonicalLedgerEventType = "main_turn_committed"
	CanonicalLedgerEventTypeSideTurnObserved                   CanonicalLedgerEventType = "side_turn_observed"
	CanonicalLedgerEventTypeCompactionApplied                  CanonicalLedgerEventType = "compaction_applied"
)

type LedgerEventBranchCreated struct {
	BranchID               string
	ParentBranchID         *string
	ForkAncestorCheckpoint *BranchCheckpointRef
	CreatedAtUnixSeconds   int64
}

type LedgerEventInboundCanonicalSnapshotReconciled struct {
	SnapshotHash         *string
	CreatedAtUnixSeconds int64
}

type LedgerEventMainTurnCommitted struct {
	TurnID               string
	ProviderResponseID   *string
	RequestFingerprint   *string
	ProviderOutputItems  []any
	CreatedAtUnixSeconds int64
}

type LedgerEventSideTurnObserved struct {
	TurnID               string
	RequestFingerprint   *string
	ProviderOutputItems  []any
	CreatedAtUnixSeconds int64
}

type LedgerEventCompactionApplied struct {
	SummaryHash          *string
	CreatedAtUnixSeconds int64
}

type CanonicalLedgerEvent struct {
	EventType                          CanonicalLedgerEventType
	BranchCreated                      *LedgerEventBranchCreated
	InboundCanonicalSnapshotReconciled *LedgerEventInboundCanonicalSnapshotReconciled
	MainTurnCommitted                  *LedgerEventMainTurnCommitted
	SideTurnObserved                   *LedgerEventSideTurnObserved
	CompactionApplied                  *LedgerEventCompactionApplied
}
