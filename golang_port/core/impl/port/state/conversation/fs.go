// Package conversation implements core/domain/port/state.ConversationRepo
// against the local filesystem, a 1:1 port of
// crates/gateway-state/src/conversation.rs (ConversationStateStore). The
// on-disk layout (session-id-<id>/, tab-<branch>/, branch.json,
// session.json, ledger.jsonl, checkpoints/<ts>-<kind>-<uuid>.json) and JSON
// field names are kept byte-compatible with what the Rust build already
// wrote to disk.
package conversation

import (
	"context"
	crand "crypto/rand"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"reflect"
	"sort"
	"strings"
	"sync"
	"time"

	"github.com/Bytonomics/cld-gateway/config"
	"github.com/Bytonomics/cld-gateway/core/domain/port/state"
)

const (
	sessionSchemaVersion              = 1
	branchSchemaVersion               = 1
	sparseCheckpointSchemaVersion     = 1
	turnOpenAICheckpointSchemaVersion = 1
)

// CorruptionPolicy mirrors gateway_core::config::ConversationStateCorruptionPolicy.
type CorruptionPolicy string

const (
	CorruptionPolicyFailClosed         CorruptionPolicy = "fail_closed"
	CorruptionPolicyQuarantineAndReset CorruptionPolicy = "quarantine_and_reset"
)

// Store implements state.ConversationRepo against the local filesystem.
type Store struct {
	root             string
	corruptionPolicy CorruptionPolicy
	clock            state.Clock
}

var _ state.ConversationRepo = (*Store)(nil)

type systemClock struct{}

func (systemClock) Now() time.Time { return time.Now() }

// New builds a Store rooted per cfg.PersistenceRoot, else
// CLD_GATEWAY_CONVERSATION_STATE_ROOT, else
// $GATEWAY_HOME|~/.gateway/sessions/claudecode (ARCHITECTURE_v2 invariant
// #10). A nil clock defaults to the system clock.
func New(cfg config.ConversationStateConfig, clock state.Clock) (*Store, error) {
	root, err := resolveConversationStateRoot(cfg)
	if err != nil {
		return nil, err
	}
	policy := CorruptionPolicy(cfg.CorruptionPolicy)
	if policy == "" {
		policy = CorruptionPolicyFailClosed
	}
	if clock == nil {
		clock = systemClock{}
	}
	return &Store{root: root, corruptionPolicy: policy, clock: clock}, nil
}

// NewWithRoot builds a Store against an explicit root, bypassing config
// resolution (ports ConversationStateStore::new / new_with_policy).
func NewWithRoot(root string, policy CorruptionPolicy, clock state.Clock) *Store {
	if policy == "" {
		policy = CorruptionPolicyFailClosed
	}
	if clock == nil {
		clock = systemClock{}
	}
	return &Store{root: root, corruptionPolicy: policy, clock: clock}
}

func resolveConversationStateRoot(cfg config.ConversationStateConfig) (string, error) {
	if cfg.PersistenceRoot != nil && *cfg.PersistenceRoot != "" {
		return *cfg.PersistenceRoot, nil
	}
	if envRoot := os.Getenv("CLD_GATEWAY_CONVERSATION_STATE_ROOT"); envRoot != "" {
		return envRoot, nil
	}
	gatewayDir, err := defaultGatewayDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(gatewayDir, "sessions", "claudecode"), nil
}

func defaultGatewayDir() (string, error) {
	if home := os.Getenv("GATEWAY_HOME"); home != "" {
		return home, nil
	}
	homeDir, err := os.UserHomeDir()
	if err != nil {
		return "", fmt.Errorf("failed to resolve home directory: %w", err)
	}
	return filepath.Join(homeDir, ".gateway"), nil
}

// Root, SessionDir, BranchDir, BranchCheckpointsDir port
// ConversationStateStore's root()/session_dir()/branch_dir()/branch_checkpoints_dir().
func (s *Store) Root() string { return s.root }

func (s *Store) SessionDir(claudeSessionID string) string {
	return filepath.Join(s.root, "session-id-"+claudeSessionID)
}

func (s *Store) BranchDir(claudeSessionID, branchID string) string {
	return filepath.Join(s.SessionDir(claudeSessionID), "tab-"+branchID)
}

func (s *Store) BranchCheckpointsDir(claudeSessionID, branchID string) string {
	return filepath.Join(s.BranchDir(claudeSessionID, branchID), "checkpoints")
}

func (s *Store) nowUnixSeconds() int64 { return s.clock.Now().Unix() }

// --- per-session lock registry (session_lock_registry parity: process-wide,
// shared across all Store instances, held for the duration of a
// read-modify-write; no blocking network calls happen under it) ---

var (
	sessionLockRegistryMu sync.Mutex
	sessionLockRegistry   = map[string]*sync.Mutex{}
)

func acquireSessionLock(sessionID string) *sync.Mutex {
	sessionLockRegistryMu.Lock()
	defer sessionLockRegistryMu.Unlock()
	lock, ok := sessionLockRegistry[sessionID]
	if !ok {
		lock = &sync.Mutex{}
		sessionLockRegistry[sessionID] = lock
	}
	return lock
}

// withSessionLock ports with_session_lock: acquire the per-session mutex,
// run operation, and on a recoverable corruption error under
// quarantine_and_reset, quarantine the session directory and retry once.
func withSessionLock[T any](s *Store, sessionID string, operation func() (T, error)) (T, error) {
	lock := acquireSessionLock(sessionID)
	lock.Lock()
	defer lock.Unlock()

	result, err := operation()
	if err != nil && s.corruptionPolicy == CorruptionPolicyQuarantineAndReset && isRecoverableStateCorruption(err) {
		if qErr := s.quarantineSessionUnlocked(sessionID, err); qErr != nil {
			var zero T
			return zero, qErr
		}
		return operation()
	}
	return result, err
}

// --- error kinds (StateError::Invariant / StateError::Json parity) ---

type invariantError struct{ msg string }

func newInvariantError(format string, args ...any) error {
	return &invariantError{msg: fmt.Sprintf(format, args...)}
}

func (e *invariantError) Error() string { return "state invariant error: " + e.msg }

type corruptJSONError struct{ cause error }

func (e *corruptJSONError) Error() string { return "json error: " + e.cause.Error() }
func (e *corruptJSONError) Unwrap() error { return e.cause }

func isRecoverableStateCorruption(err error) bool {
	var inv *invariantError
	var je *corruptJSONError
	return errors.As(err, &inv) || errors.As(err, &je)
}

// --- session ---

func (s *Store) EnsureSession(ctx context.Context, claudeSessionID string) (*state.ClaudeSessionMetadata, error) {
	return withSessionLock(s, claudeSessionID, func() (*state.ClaudeSessionMetadata, error) {
		return s.ensureSessionUnlocked(claudeSessionID)
	})
}

func (s *Store) ensureSessionUnlocked(claudeSessionID string) (*state.ClaudeSessionMetadata, error) {
	sessionDir := s.SessionDir(claudeSessionID)
	if err := os.MkdirAll(sessionDir, 0o755); err != nil {
		return nil, err
	}
	sessionPath := filepath.Join(sessionDir, "session.json")
	if _, err := os.Stat(sessionPath); err == nil {
		return s.loadSessionUnlocked(claudeSessionID)
	} else if !errors.Is(err, os.ErrNotExist) {
		return nil, err
	}

	now := s.nowUnixSeconds()
	session := state.ClaudeSessionMetadata{
		SchemaVersion:        sessionSchemaVersion,
		ClaudeSessionID:      claudeSessionID,
		CreatedAtUnixSeconds: now,
		UpdatedAtUnixSeconds: now,
		BranchIDs:            []string{},
	}
	if err := s.writeSession(session); err != nil {
		return nil, err
	}
	return &session, nil
}

func (s *Store) LoadSession(ctx context.Context, claudeSessionID string) (*state.ClaudeSessionMetadata, error) {
	return s.loadSessionUnlocked(claudeSessionID)
}

func (s *Store) loadSessionUnlocked(claudeSessionID string) (*state.ClaudeSessionMetadata, error) {
	path := filepath.Join(s.SessionDir(claudeSessionID), "session.json")
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}
	var w sessionWire
	if err := json.Unmarshal(data, &w); err != nil {
		return nil, &corruptJSONError{cause: err}
	}
	session := sessionFromWire(w)
	return &session, nil
}

func (s *Store) writeSession(session state.ClaudeSessionMetadata) error {
	path := filepath.Join(s.SessionDir(session.ClaudeSessionID), "session.json")
	return writeJSONAtomically(path, sessionToWire(session))
}

func (s *Store) LoadAllBranches(ctx context.Context, claudeSessionID string) ([]state.BranchMetadata, error) {
	session, err := s.ensureSessionUnlocked(claudeSessionID)
	if err != nil {
		return nil, err
	}
	branches := make([]state.BranchMetadata, 0, len(session.BranchIDs))
	for _, branchID := range session.BranchIDs {
		branch, err := s.loadBranchUnlocked(claudeSessionID, branchID)
		if err != nil {
			return nil, err
		}
		branches = append(branches, branch)
	}
	return branches, nil
}

func (s *Store) loadAllBranchesForSelectionUnlocked(claudeSessionID string) ([]state.BranchMetadata, error) {
	session, err := s.ensureSessionUnlocked(claudeSessionID)
	if err != nil {
		return nil, err
	}
	branches := make([]state.BranchMetadata, 0, len(session.BranchIDs))
	for _, branchID := range session.BranchIDs {
		branch, err := s.rebuildBranchFromDiskUnlocked(claudeSessionID, branchID)
		if err != nil {
			return nil, err
		}
		branches = append(branches, branch)
	}
	return branches, nil
}

func (s *Store) CleanupSessionsOlderThan(ctx context.Context, days int) (int, error) {
	if days < 0 {
		return 0, nil
	}
	maxAgeSeconds := int64(days) * 24 * 60 * 60
	cutoff := s.nowUnixSeconds() - maxAgeSeconds
	return s.cleanupSessionsOlderThanUnixSeconds(cutoff)
}

func (s *Store) cleanupSessionsOlderThanUnixSeconds(cutoffUnixSeconds int64) (int, error) {
	if _, err := os.Stat(s.root); err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return 0, nil
		}
		return 0, err
	}

	entries, err := os.ReadDir(s.root)
	if err != nil {
		return 0, err
	}

	removed := 0
	for _, entry := range entries {
		if !entry.IsDir() {
			continue
		}
		claudeSessionID, ok := strings.CutPrefix(entry.Name(), "session-id-")
		if !ok {
			continue
		}
		session, err := s.loadSessionUnlocked(claudeSessionID)
		if err != nil {
			return 0, err
		}
		if session.UpdatedAtUnixSeconds > cutoffUnixSeconds {
			continue
		}
		if err := os.RemoveAll(filepath.Join(s.root, entry.Name())); err != nil {
			return 0, err
		}
		removed++
	}
	return removed, nil
}

// --- branch ---

func (s *Store) CreateBranch(ctx context.Context, claudeSessionID string, p state.BranchCreateParams) (state.BranchMetadata, error) {
	return withSessionLock(s, claudeSessionID, func() (state.BranchMetadata, error) {
		return s.createBranchUnlocked(claudeSessionID, p)
	})
}

func (s *Store) createBranchUnlocked(claudeSessionID string, params state.BranchCreateParams) (state.BranchMetadata, error) {
	session, err := s.ensureSessionUnlocked(claudeSessionID)
	if err != nil {
		return state.BranchMetadata{}, err
	}

	branchID := newUUIDv4()
	branchDir := s.BranchDir(claudeSessionID, branchID)
	if err := os.MkdirAll(filepath.Join(branchDir, "checkpoints"), 0o755); err != nil {
		return state.BranchMetadata{}, err
	}
	now := s.nowUnixSeconds()

	branch := state.BranchMetadata{
		SchemaVersion:             branchSchemaVersion,
		BranchID:                  branchID,
		ParentBranchID:            params.ParentBranchID,
		ForkAncestorCheckpoint:    params.ForkAncestorCheckpoint,
		CurrentCheckpointID:       nil,
		ActiveCanonicalMessages:   params.ActiveCanonicalMessages,
		Fingerprints:              params.Fingerprints,
		OpenAiCheckpoint:          nil,
		TurnOpenAiCheckpoints:     []state.TurnOpenAiCheckpoint{},
		OffshootOpenAiCheckpoints: []state.OffshootOpenAiCheckpoint{},
		CompactionResetPending:    false,
		LastMainTurnID:            nil,
		CreatedAtUnixSeconds:      now,
		UpdatedAtUnixSeconds:      now,
	}

	branchPath := filepath.Join(branchDir, "branch.json")
	if err := writeJSONAtomically(branchPath, branchToWire(branch)); err != nil {
		return state.BranchMetadata{}, err
	}

	branchCreated := state.CanonicalLedgerEvent{
		EventType: state.CanonicalLedgerEventTypeBranchCreated,
		BranchCreated: &state.LedgerEventBranchCreated{
			BranchID:               branchID,
			ParentBranchID:         params.ParentBranchID,
			ForkAncestorCheckpoint: params.ForkAncestorCheckpoint,
			CreatedAtUnixSeconds:   now,
		},
	}
	if err := s.appendLedgerEventUnlocked(claudeSessionID, branchID, branchCreated); err != nil {
		return state.BranchMetadata{}, err
	}

	kind := state.SparseCheckpointKindBranchCreated
	if params.ParentBranchID != nil {
		kind = state.SparseCheckpointKindBranchForkCreated
	}
	if err := s.writeSparseCheckpointUnlocked(claudeSessionID, branch, kind, now); err != nil {
		return state.BranchMetadata{}, err
	}

	session.BranchIDs = append(session.BranchIDs, branchID)
	session.UpdatedAtUnixSeconds = now
	if err := s.writeSession(*session); err != nil {
		return state.BranchMetadata{}, err
	}

	return branch, nil
}

func (s *Store) SelectOrCreateBranch(ctx context.Context, claudeSessionID string, in state.BranchSelectionInput) (state.BranchSelectionResult, error) {
	return withSessionLock(s, claudeSessionID, func() (state.BranchSelectionResult, error) {
		return s.selectOrCreateBranchUnlocked(claudeSessionID, in)
	})
}

func (s *Store) selectOrCreateBranchUnlocked(claudeSessionID string, input state.BranchSelectionInput) (state.BranchSelectionResult, error) {
	existing, err := s.loadAllBranchesForSelectionUnlocked(claudeSessionID)
	if err != nil {
		return state.BranchSelectionResult{}, err
	}

	if len(existing) == 0 {
		branch, err := s.createBranchUnlocked(claudeSessionID, state.BranchCreateParams{
			ActiveCanonicalMessages: input.ActiveCanonicalMessages,
			Fingerprints:            input.Fingerprints,
		})
		if err != nil {
			return state.BranchSelectionResult{}, err
		}
		return state.BranchSelectionResult{
			Branch: branch,
			Action: state.BranchSelectionActionCreatedInitial,
		}, nil
	}

	var exactMatches []state.BranchMetadata
	for _, b := range existing {
		if exactBranchMatch(b.Fingerprints, input.Fingerprints) {
			exactMatches = append(exactMatches, b)
		}
	}

	if len(exactMatches) == 1 {
		previous := exactMatches[0]
		branch := previous
		branch.ActiveCanonicalMessages = input.ActiveCanonicalMessages
		branch.Fingerprints = input.Fingerprints
		if err := s.storeBranchUnlocked(claudeSessionID, branch); err != nil {
			return state.BranchSelectionResult{}, err
		}
		matched := previous
		return state.BranchSelectionResult{
			Branch:                branch,
			Action:                state.BranchSelectionActionContinuedExisting,
			MatchedExistingBranch: &matched,
		}, nil
	}
	if len(exactMatches) > 1 {
		branch, err := s.createBranchUnlocked(claudeSessionID, state.BranchCreateParams{
			ActiveCanonicalMessages: input.ActiveCanonicalMessages,
			Fingerprints:            input.Fingerprints,
		})
		if err != nil {
			return state.BranchSelectionResult{}, err
		}
		return state.BranchSelectionResult{
			Branch: branch,
			Action: state.BranchSelectionActionCreatedAmbiguous,
		}, nil
	}

	var ancestorCandidates []state.BranchMetadata
	for _, b := range existing {
		if ancestorBranchMatch(b.Fingerprints, input.Fingerprints) {
			ancestorCandidates = append(ancestorCandidates, b)
		}
	}

	if len(ancestorCandidates) == 1 {
		ancestor := ancestorCandidates[0]
		var forkRef *state.BranchCheckpointRef
		if ancestor.CurrentCheckpointID != nil {
			forkRef = &state.BranchCheckpointRef{
				BranchID:     ancestor.BranchID,
				CheckpointID: *ancestor.CurrentCheckpointID,
			}
		}
		ancestorID := ancestor.BranchID
		branch, err := s.createBranchUnlocked(claudeSessionID, state.BranchCreateParams{
			ParentBranchID:          &ancestorID,
			ForkAncestorCheckpoint:  forkRef,
			ActiveCanonicalMessages: ancestor.ActiveCanonicalMessages,
			Fingerprints:            input.Fingerprints,
		})
		if err != nil {
			return state.BranchSelectionResult{}, err
		}
		matched := ancestor
		return state.BranchSelectionResult{
			Branch:                branch,
			Action:                state.BranchSelectionActionForkedFromAncestor,
			MatchedExistingBranch: &matched,
		}, nil
	}

	action := state.BranchSelectionActionCreatedUnmatched
	if len(ancestorCandidates) != 0 {
		action = state.BranchSelectionActionCreatedAmbiguous
	}
	branch, err := s.createBranchUnlocked(claudeSessionID, state.BranchCreateParams{
		ActiveCanonicalMessages: input.ActiveCanonicalMessages,
		Fingerprints:            input.Fingerprints,
	})
	if err != nil {
		return state.BranchSelectionResult{}, err
	}
	return state.BranchSelectionResult{Branch: branch, Action: action}, nil
}

func (s *Store) LoadBranch(ctx context.Context, claudeSessionID, branchID string) (*state.BranchMetadata, error) {
	branch, err := s.loadBranchUnlocked(claudeSessionID, branchID)
	if err != nil {
		return nil, err
	}
	return &branch, nil
}

func (s *Store) loadBranchUnlocked(claudeSessionID, branchID string) (state.BranchMetadata, error) {
	path := filepath.Join(s.BranchDir(claudeSessionID, branchID), "branch.json")
	data, err := os.ReadFile(path)
	if err != nil {
		return state.BranchMetadata{}, err
	}
	var w branchWire
	if err := json.Unmarshal(data, &w); err != nil {
		return state.BranchMetadata{}, &corruptJSONError{cause: err}
	}
	return branchFromWire(w), nil
}

func (s *Store) StoreBranch(ctx context.Context, claudeSessionID string, b state.BranchMetadata) error {
	_, err := withSessionLock(s, claudeSessionID, func() (struct{}, error) {
		return struct{}{}, s.storeBranchUnlocked(claudeSessionID, b)
	})
	return err
}

func (s *Store) storeBranchUnlocked(claudeSessionID string, branch state.BranchMetadata) error {
	updated := branch
	updated.UpdatedAtUnixSeconds = s.nowUnixSeconds()
	path := filepath.Join(s.BranchDir(claudeSessionID, branch.BranchID), "branch.json")
	return writeJSONAtomically(path, branchToWire(updated))
}

// --- ledger ---

func (s *Store) AppendLedgerEvent(ctx context.Context, claudeSessionID, branchID string, ev state.CanonicalLedgerEvent) error {
	_, err := withSessionLock(s, claudeSessionID, func() (struct{}, error) {
		return struct{}{}, s.appendLedgerEventUnlocked(claudeSessionID, branchID, ev)
	})
	return err
}

func (s *Store) appendLedgerEventUnlocked(claudeSessionID, branchID string, ev state.CanonicalLedgerEvent) error {
	ledgerPath := filepath.Join(s.BranchDir(claudeSessionID, branchID), "ledger.jsonl")
	if err := os.MkdirAll(filepath.Dir(ledgerPath), 0o755); err != nil {
		return err
	}
	f, err := os.OpenFile(ledgerPath, os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0o644)
	if err != nil {
		return err
	}
	defer func() { _ = f.Close() }()

	encoded, err := marshalLedgerEvent(ev)
	if err != nil {
		return err
	}
	if _, err := f.Write(encoded); err != nil {
		return err
	}
	if _, err := f.Write([]byte("\n")); err != nil {
		return err
	}
	return nil
}

func (s *Store) loadBranchLedgerEvents(claudeSessionID, branchID string) ([]state.CanonicalLedgerEvent, error) {
	ledgerPath := filepath.Join(s.BranchDir(claudeSessionID, branchID), "ledger.jsonl")
	text, err := os.ReadFile(ledgerPath)
	if err != nil {
		return nil, err
	}
	lines := strings.Split(string(text), "\n")
	events := make([]state.CanonicalLedgerEvent, 0, len(lines))
	for _, line := range lines {
		if strings.TrimSpace(line) == "" {
			continue
		}
		ev, err := unmarshalLedgerEvent([]byte(line))
		if err != nil {
			return nil, &corruptJSONError{cause: err}
		}
		events = append(events, ev)
	}
	return events, nil
}

// --- turn / checkpoint commits ---

func (s *Store) CommitTurn(ctx context.Context, claudeSessionID, branchID string, p state.CommitTurnParams) (state.BranchMetadata, error) {
	return withSessionLock(s, claudeSessionID, func() (state.BranchMetadata, error) {
		return s.commitTurnUnlocked(claudeSessionID, branchID, p)
	})
}

func (s *Store) commitTurnUnlocked(claudeSessionID, branchID string, params state.CommitTurnParams) (state.BranchMetadata, error) {
	branch, err := s.loadBranchUnlocked(claudeSessionID, branchID)
	if err != nil {
		return state.BranchMetadata{}, err
	}
	branch.Fingerprints = params.Fingerprints
	if params.ActiveCanonicalMessages != nil {
		branch.ActiveCanonicalMessages = params.ActiveCanonicalMessages
	}
	now := s.nowUnixSeconds()
	branch.UpdatedAtUnixSeconds = now

	if params.TurnScope == state.ConversationTurnScopeMain {
		turnID := params.TurnID
		branch.LastMainTurnID = &turnID
		if params.ProviderResponseID != nil && params.ProviderModelFingerprint != nil {
			responseID := *params.ProviderResponseID
			modelFingerprint := *params.ProviderModelFingerprint
			branch.CurrentCheckpointID = &responseID
			branch.CompactionResetPending = false
			branch.OpenAiCheckpoint = &state.OpenAiCheckpoint{
				ResponseID:                      responseID,
				PreviousResponseID:              params.PreviousResponseID,
				ProviderModelFingerprint:        modelFingerprint,
				RequestCompatibilityFingerprint: params.RequestCompatibilityFingerprint,
				ProviderInputTokens:             params.ProviderInputTokens,
			}
			if params.CanonicalMessageCount != nil && params.CanonicalPrefixHash != nil {
				count := *params.CanonicalMessageCount
				hash := *params.CanonicalPrefixHash
				var filtered []state.TurnOpenAiCheckpoint
				for _, cp := range branch.TurnOpenAiCheckpoints {
					if cp.CanonicalMessageCount != count || cp.CanonicalPrefixHash != hash {
						filtered = append(filtered, cp)
					}
				}
				filtered = append(filtered, state.TurnOpenAiCheckpoint{
					SchemaVersion:                   turnOpenAICheckpointSchemaVersion,
					TurnID:                          params.TurnID,
					CanonicalMessageCount:           count,
					CanonicalPrefixHash:             hash,
					ResponseID:                      responseID,
					PreviousResponseID:              params.PreviousResponseID,
					ProviderModelFingerprint:        modelFingerprint,
					RequestCompatibilityFingerprint: params.RequestCompatibilityFingerprint,
					ProviderInputTokens:             params.ProviderInputTokens,
					CreatedAtUnixSeconds:            now,
				})
				sort.SliceStable(filtered, func(i, j int) bool {
					if filtered[i].CanonicalMessageCount != filtered[j].CanonicalMessageCount {
						return filtered[i].CanonicalMessageCount < filtered[j].CanonicalMessageCount
					}
					return filtered[i].CreatedAtUnixSeconds < filtered[j].CreatedAtUnixSeconds
				})
				if filtered == nil {
					filtered = []state.TurnOpenAiCheckpoint{}
				}
				branch.TurnOpenAiCheckpoints = filtered
			}
		}
	}

	var event state.CanonicalLedgerEvent
	if params.TurnScope == state.ConversationTurnScopeMain {
		event = state.CanonicalLedgerEvent{
			EventType: state.CanonicalLedgerEventTypeMainTurnCommitted,
			MainTurnCommitted: &state.LedgerEventMainTurnCommitted{
				TurnID:               params.TurnID,
				ProviderResponseID:   params.ProviderResponseID,
				RequestFingerprint:   branch.Fingerprints.RecentMessageTailHash,
				ProviderOutputItems:  params.ProviderOutputItems,
				CreatedAtUnixSeconds: now,
			},
		}
	} else {
		event = state.CanonicalLedgerEvent{
			EventType: state.CanonicalLedgerEventTypeSideTurnObserved,
			SideTurnObserved: &state.LedgerEventSideTurnObserved{
				TurnID:               params.TurnID,
				RequestFingerprint:   branch.Fingerprints.RecentMessageTailHash,
				ProviderOutputItems:  params.ProviderOutputItems,
				CreatedAtUnixSeconds: now,
			},
		}
	}

	if err := s.appendLedgerEventUnlocked(claudeSessionID, branchID, event); err != nil {
		return state.BranchMetadata{}, err
	}
	if err := s.storeBranchUnlocked(claudeSessionID, branch); err != nil {
		return state.BranchMetadata{}, err
	}
	return branch, nil
}

func (s *Store) CommitOffshootCheckpoint(ctx context.Context, claudeSessionID, branchID string, p state.CommitOffshootCheckpointParams) error {
	_, err := withSessionLock(s, claudeSessionID, func() (state.BranchMetadata, error) {
		return s.commitOffshootOpenAICheckpointUnlocked(claudeSessionID, branchID, p)
	})
	return err
}

func (s *Store) commitOffshootOpenAICheckpointUnlocked(claudeSessionID, branchID string, params state.CommitOffshootCheckpointParams) (state.BranchMetadata, error) {
	branch, err := s.loadBranchUnlocked(claudeSessionID, branchID)
	if err != nil {
		return state.BranchMetadata{}, err
	}
	now := s.nowUnixSeconds()
	branch.UpdatedAtUnixSeconds = now

	var filtered []state.OffshootOpenAiCheckpoint
	for _, cp := range branch.OffshootOpenAiCheckpoints {
		if cp.OffshootIdentity != params.OffshootIdentity {
			filtered = append(filtered, cp)
		}
	}
	filtered = append(filtered, state.OffshootOpenAiCheckpoint{
		SchemaVersion:                   turnOpenAICheckpointSchemaVersion,
		OffshootIdentity:                params.OffshootIdentity,
		ResponseID:                      params.ProviderResponseID,
		PreviousResponseID:              params.PreviousResponseID,
		ProviderModelFingerprint:        params.ProviderModelFingerprint,
		RequestCompatibilityFingerprint: params.RequestCompatibilityFingerprint,
		ProviderInputTokens:             params.ProviderInputTokens,
		CreatedAtUnixSeconds:            now,
	})
	sort.SliceStable(filtered, func(i, j int) bool {
		return filtered[i].CreatedAtUnixSeconds < filtered[j].CreatedAtUnixSeconds
	})
	branch.OffshootOpenAiCheckpoints = filtered

	if err := s.storeBranchUnlocked(claudeSessionID, branch); err != nil {
		return state.BranchMetadata{}, err
	}
	return branch, nil
}

func (s *Store) ReconcileSnapshot(ctx context.Context, claudeSessionID, branchID string, p state.ReconcileSnapshotParams) (state.BranchMetadata, error) {
	return withSessionLock(s, claudeSessionID, func() (state.BranchMetadata, error) {
		return s.reconcileBranchSnapshotUnlocked(claudeSessionID, branchID, p)
	})
}

func (s *Store) reconcileBranchSnapshotUnlocked(claudeSessionID, branchID string, params state.ReconcileSnapshotParams) (state.BranchMetadata, error) {
	branch, err := s.loadBranchUnlocked(claudeSessionID, branchID)
	if err != nil {
		return state.BranchMetadata{}, err
	}
	snapshotChanged := !reflect.DeepEqual(branch.ActiveCanonicalMessages, params.Messages)
	branch.ActiveCanonicalMessages = params.Messages
	branch.Fingerprints = params.Fingerprints
	branch.UpdatedAtUnixSeconds = s.nowUnixSeconds()

	if snapshotChanged {
		event := state.CanonicalLedgerEvent{
			EventType: state.CanonicalLedgerEventTypeInboundCanonicalSnapshotReconciled,
			InboundCanonicalSnapshotReconciled: &state.LedgerEventInboundCanonicalSnapshotReconciled{
				SnapshotHash:         params.Fingerprints.BranchStateHash,
				CreatedAtUnixSeconds: s.nowUnixSeconds(),
			},
		}
		if err := s.appendLedgerEventUnlocked(claudeSessionID, branchID, event); err != nil {
			return state.BranchMetadata{}, err
		}
	}
	if err := s.storeBranchUnlocked(claudeSessionID, branch); err != nil {
		return state.BranchMetadata{}, err
	}
	return branch, nil
}

// ApplyCompaction deviates from conversation.rs:862-907's apply_compaction,
// which also takes a fingerprints parameter and overwrites
// branch.fingerprints with it. The pinned ConversationRepo interface
// (core/domain/port/state/conversation.go) only carries summaryHash, so
// this leaves the branch's existing fingerprints unchanged. An empty
// summaryHash is treated as Rust's None.
func (s *Store) ApplyCompaction(ctx context.Context, claudeSessionID, branchID string, summaryHash string) (state.BranchMetadata, error) {
	return withSessionLock(s, claudeSessionID, func() (state.BranchMetadata, error) {
		return s.applyCompactionUnlocked(claudeSessionID, branchID, summaryHash)
	})
}

func (s *Store) applyCompactionUnlocked(claudeSessionID, branchID, summaryHash string) (state.BranchMetadata, error) {
	branch, err := s.loadBranchUnlocked(claudeSessionID, branchID)
	if err != nil {
		return state.BranchMetadata{}, err
	}
	branch.CompactionResetPending = true
	now := s.nowUnixSeconds()
	branch.UpdatedAtUnixSeconds = now

	var hashPtr *string
	if summaryHash != "" {
		hashPtr = &summaryHash
	}
	event := state.CanonicalLedgerEvent{
		EventType: state.CanonicalLedgerEventTypeCompactionApplied,
		CompactionApplied: &state.LedgerEventCompactionApplied{
			SummaryHash:          hashPtr,
			CreatedAtUnixSeconds: now,
		},
	}
	if err := s.appendLedgerEventUnlocked(claudeSessionID, branchID, event); err != nil {
		return state.BranchMetadata{}, err
	}
	if err := s.storeBranchUnlocked(claudeSessionID, branch); err != nil {
		return state.BranchMetadata{}, err
	}
	if err := s.writeSparseCheckpointUnlocked(claudeSessionID, branch, state.SparseCheckpointKindCompactionApplied, now); err != nil {
		return state.BranchMetadata{}, err
	}
	return branch, nil
}

// InvalidateCheckpoint discards the returned BranchMetadata that Rust's
// invalidate_openai_checkpoint produces: the pinned interface signature
// returns only an error.
func (s *Store) InvalidateCheckpoint(ctx context.Context, claudeSessionID, branchID string) error {
	_, err := withSessionLock(s, claudeSessionID, func() (state.BranchMetadata, error) {
		return s.invalidateOpenAICheckpointUnlocked(claudeSessionID, branchID)
	})
	return err
}

func (s *Store) invalidateOpenAICheckpointUnlocked(claudeSessionID, branchID string) (state.BranchMetadata, error) {
	branch, err := s.loadBranchUnlocked(claudeSessionID, branchID)
	if err != nil {
		return state.BranchMetadata{}, err
	}
	branch.CurrentCheckpointID = nil
	branch.OpenAiCheckpoint = nil
	branch.UpdatedAtUnixSeconds = s.nowUnixSeconds()
	if err := s.storeBranchUnlocked(claudeSessionID, branch); err != nil {
		return state.BranchMetadata{}, err
	}
	return branch, nil
}

func (s *Store) RebuildBranchFromDisk(ctx context.Context, claudeSessionID, branchID string) (state.BranchMetadata, error) {
	return s.rebuildBranchFromDiskUnlocked(claudeSessionID, branchID)
}

func (s *Store) rebuildBranchFromDiskUnlocked(claudeSessionID, branchID string) (state.BranchMetadata, error) {
	branch, err := s.loadBranchUnlocked(claudeSessionID, branchID)
	if err != nil {
		return state.BranchMetadata{}, err
	}
	events, err := s.loadBranchLedgerEvents(claudeSessionID, branchID)
	if err != nil {
		return state.BranchMetadata{}, err
	}
	if err := validateBranchAgainstLedger(branch, events); err != nil {
		return state.BranchMetadata{}, err
	}
	return branch, nil
}

// FindTurnCheckpoint deviates from conversation.rs:947-962's
// find_turn_openai_checkpoint, a static lookup by
// (canonical_message_count, canonical_prefix_hash) against an in-memory
// BranchMetadata. The pinned interface instead passes a turnID and no
// BranchMetadata, so this loads the branch itself and matches on TurnID,
// keeping the max-by-created-at tie-break for multiple matches.
func (s *Store) FindTurnCheckpoint(ctx context.Context, claudeSessionID, branchID, turnID string) (*state.TurnOpenAiCheckpoint, bool) {
	branch, err := s.loadBranchUnlocked(claudeSessionID, branchID)
	if err != nil {
		return nil, false
	}
	var best *state.TurnOpenAiCheckpoint
	for i := range branch.TurnOpenAiCheckpoints {
		cp := branch.TurnOpenAiCheckpoints[i]
		if cp.TurnID != turnID {
			continue
		}
		if best == nil || cp.CreatedAtUnixSeconds > best.CreatedAtUnixSeconds {
			cpCopy := cp
			best = &cpCopy
		}
	}
	return best, best != nil
}

// --- sparse checkpoints ---

var sparseCheckpointKindDebugLower = map[state.SparseCheckpointKind]string{
	state.SparseCheckpointKindBranchCreated:     "branchcreated",
	state.SparseCheckpointKindBranchForkCreated: "branchforkcreated",
	state.SparseCheckpointKindCompactionApplied: "compactionapplied",
}

func (s *Store) writeSparseCheckpointUnlocked(claudeSessionID string, branch state.BranchMetadata, kind state.SparseCheckpointKind, createdAtUnixSeconds int64) error {
	checkpoint := sparseCheckpointWire{
		SchemaVersion:           sparseCheckpointSchemaVersion,
		CheckpointKind:          string(kind),
		BranchID:                branch.BranchID,
		CurrentCheckpointID:     branch.CurrentCheckpointID,
		ParentBranchID:          branch.ParentBranchID,
		ForkAncestorCheckpoint:  toBranchCheckpointRefWire(branch.ForkAncestorCheckpoint),
		ActiveCanonicalMessages: branch.ActiveCanonicalMessages,
		Fingerprints:            fingerprintsToWire(branch.Fingerprints),
		CompactionResetPending:  branch.CompactionResetPending,
		LastMainTurnID:          branch.LastMainTurnID,
		CreatedAtUnixSeconds:    createdAtUnixSeconds,
	}
	fileName := strings.ToLower(fmt.Sprintf("%020d-%s-%s.json", createdAtUnixSeconds, sparseCheckpointKindDebugLower[kind], newUUIDv4()))
	path := filepath.Join(s.BranchCheckpointsDir(claudeSessionID, branch.BranchID), fileName)
	return writeJSONAtomically(path, checkpoint)
}

// --- quarantine (fail_closed vs quarantine_and_reset corruption policy) ---

func (s *Store) quarantineRoot() string {
	parent := filepath.Dir(s.root)
	rootName := filepath.Base(s.root)
	if rootName == "" || rootName == "." || rootName == string(filepath.Separator) {
		rootName = "claudecode"
	}
	return filepath.Join(parent, rootName+"-quarantine")
}

func (s *Store) quarantineSessionUnlocked(claudeSessionID string, reason error) error {
	sessionDir := s.SessionDir(claudeSessionID)
	if _, err := os.Stat(sessionDir); err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return nil
		}
		return err
	}
	quarantineRoot := s.quarantineRoot()
	if err := os.MkdirAll(quarantineRoot, 0o755); err != nil {
		return err
	}
	quarantineDir := filepath.Join(quarantineRoot, fmt.Sprintf("session-id-%s-%d-%s", claudeSessionID, s.nowUnixSeconds(), newUUIDv4()))
	if err := os.Rename(sessionDir, quarantineDir); err != nil {
		return err
	}
	metadataPath := filepath.Join(quarantineDir, "quarantine-metadata.json")
	metadata := map[string]any{
		"claude_session_id":           claudeSessionID,
		"reason":                      reason.Error(),
		"quarantined_at_unix_seconds": s.nowUnixSeconds(),
	}
	return writeJSONAtomically(metadataPath, metadata)
}

// --- fingerprint matching (exact_branch_match / ancestor_branch_match parity) ---

func exactBranchMatch(existing, incoming state.BranchFingerprintSet) bool {
	return fingerprintEqual(existing.LastUserMessageHash, incoming.LastUserMessageHash) &&
		fingerprintEqual(existing.RecentMessageTailHash, incoming.RecentMessageTailHash)
}

func ancestorBranchMatch(existing, incoming state.BranchFingerprintSet) bool {
	return fingerprintEqual(existing.LastUserMessageHash, incoming.LastUserMessageHash) &&
		!stringPtrEqual(existing.RecentMessageTailHash, incoming.RecentMessageTailHash)
}

// fingerprintEqual requires both sides present and equal (Rust: only
// Some(x) == Some(y) matches; None on either side is not a match).
func fingerprintEqual(a, b *string) bool {
	if a == nil || b == nil {
		return false
	}
	return *a == *b
}

// stringPtrEqual is Option<String> equality (None == None is true).
func stringPtrEqual(a, b *string) bool {
	if a == nil && b == nil {
		return true
	}
	if a == nil || b == nil {
		return false
	}
	return *a == *b
}

// --- ledger validation (validate_branch_against_ledger parity) ---

func validateBranchAgainstLedger(branch state.BranchMetadata, events []state.CanonicalLedgerEvent) error {
	if len(events) == 0 || events[0].EventType != state.CanonicalLedgerEventTypeBranchCreated {
		return newInvariantError("branch ledger is missing its initial branch_created event")
	}

	var lastMainTurnID *string
	var lastProviderResponseID *string
	compactionResetPending := false

	for _, ev := range events {
		switch ev.EventType {
		case state.CanonicalLedgerEventTypeMainTurnCommitted:
			p := ev.MainTurnCommitted
			turnID := p.TurnID
			lastMainTurnID = &turnID
			lastProviderResponseID = p.ProviderResponseID
			compactionResetPending = false
		case state.CanonicalLedgerEventTypeCompactionApplied:
			compactionResetPending = true
		}
	}

	if !stringPtrEqual(branch.LastMainTurnID, lastMainTurnID) {
		return newInvariantError("branch last_main_turn_id %s does not match ledger %s", strPtrDebug(branch.LastMainTurnID), strPtrDebug(lastMainTurnID))
	}
	if !stringPtrEqual(branch.CurrentCheckpointID, lastProviderResponseID) {
		return newInvariantError("branch current_checkpoint_id %s does not match ledger %s", strPtrDebug(branch.CurrentCheckpointID), strPtrDebug(lastProviderResponseID))
	}
	if branch.CompactionResetPending != compactionResetPending {
		return newInvariantError("branch compaction_reset_pending %t does not match ledger %t", branch.CompactionResetPending, compactionResetPending)
	}
	if branch.CurrentCheckpointID == nil && branch.OpenAiCheckpoint != nil {
		return newInvariantError("branch has OpenAI checkpoint metadata without a current checkpoint id")
	}
	if branch.OpenAiCheckpoint != nil {
		if branch.CurrentCheckpointID == nil || branch.OpenAiCheckpoint.ResponseID != *branch.CurrentCheckpointID {
			return newInvariantError("branch checkpoint response_id %s does not match current checkpoint %s", fmt.Sprintf("%q", branch.OpenAiCheckpoint.ResponseID), strPtrDebug(branch.CurrentCheckpointID))
		}
	}
	return nil
}

func strPtrDebug(p *string) string {
	if p == nil {
		return "<nil>"
	}
	return fmt.Sprintf("%q", *p)
}

// --- atomic JSON persistence + UUIDv4 (write_json_atomically / Uuid::new_v4 parity) ---

func writeJSONAtomically(path string, v any) error {
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return err
	}
	encoded, err := json.MarshalIndent(v, "", "  ")
	if err != nil {
		return err
	}
	tmpPath := tmpPathFor(path)
	if err := os.WriteFile(tmpPath, encoded, 0o644); err != nil {
		return err
	}
	return os.Rename(tmpPath, path)
}

func tmpPathFor(path string) string {
	ext := filepath.Ext(path)
	return strings.TrimSuffix(path, ext) + ".tmp"
}

func newUUIDv4() string {
	b := make([]byte, 16)
	_, _ = crand.Read(b)
	b[6] = (b[6] & 0x0f) | 0x40
	b[8] = (b[8] & 0x3f) | 0x80
	return fmt.Sprintf("%x-%x-%x-%x-%x", b[0:4], b[4:6], b[6:8], b[8:10], b[10:16])
}

func nonNilAnySlice(v []any) []any {
	if v == nil {
		return []any{}
	}
	return v
}
