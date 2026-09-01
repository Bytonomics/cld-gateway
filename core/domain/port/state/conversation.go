package state

import "context"

// ConversationRepo is a 1:1 port of the 20 ConversationStateStore methods
// (crates/gateway-state/src/conversation.rs). Interface only; the
// filesystem implementation lands in a later wave.
type ConversationRepo interface {
	EnsureSession(ctx context.Context, sessionID string) (*ClaudeSessionMetadata, error)
	LoadSession(ctx context.Context, sessionID string) (*ClaudeSessionMetadata, error)
	LoadAllBranches(ctx context.Context, sessionID string) ([]BranchMetadata, error)
	CreateBranch(ctx context.Context, sessionID string, p BranchCreateParams) (BranchMetadata, error)
	SelectOrCreateBranch(ctx context.Context, sessionID string, in BranchSelectionInput) (BranchSelectionResult, error)
	LoadBranch(ctx context.Context, sessionID, branchID string) (*BranchMetadata, error)
	StoreBranch(ctx context.Context, sessionID string, b BranchMetadata) error
	AppendLedgerEvent(ctx context.Context, sessionID, branchID string, ev CanonicalLedgerEvent) error
	CommitTurn(ctx context.Context, sessionID, branchID string, p CommitTurnParams) (BranchMetadata, error)
	CommitOffshootCheckpoint(ctx context.Context, sessionID, branchID string, p CommitOffshootCheckpointParams) error
	ReconcileSnapshot(ctx context.Context, sessionID, branchID string, p ReconcileSnapshotParams) (BranchMetadata, error)
	ApplyCompaction(ctx context.Context, sessionID, branchID string, summaryHash string, fingerprints BranchFingerprintSet) (BranchMetadata, error)
	InvalidateCheckpoint(ctx context.Context, sessionID, branchID string) error
	RebuildBranchFromDisk(ctx context.Context, sessionID, branchID string) (BranchMetadata, error)
	FindTurnCheckpoint(ctx context.Context, sessionID, branchID string, canonicalMessageCount uint64, canonicalPrefixHash string) (*TurnOpenAiCheckpoint, bool)
	CleanupSessionsOlderThan(ctx context.Context, days int) (int, error)
}
