package transport

import (
	"errors"
	"fmt"
	"sync"

	"github.com/Bytonomics/cld-gateway/core/domain/conversation"
	"github.com/Bytonomics/cld-gateway/core/domain/port/backend"
)

type LeaseState string

const (
	InFlight                        LeaseState = "in_flight"
	CompletedCommitted              LeaseState = "completed_committed"
	ClientAbortedBeforeFirstEvent   LeaseState = "client_aborted_before_first_event"
	ClientAbortedAfterVisibleOutput LeaseState = "client_aborted_after_visible_output"
	BackendFailedBeforeCommit       LeaseState = "backend_failed_before_commit"
	CommitSuppressedAfterAbort      LeaseState = "commit_suppressed_after_abort"
)

func (s LeaseState) AllowsCommit() bool {
	return s == InFlight
}

var (
	ErrLeaseMissing         = errors.New("missing_active_lease")
	ErrLeaseRequestMismatch = errors.New("request_id_mismatch")
)

// LeaseCommitResult mirrors MainTurnLeaseCommit (lib.rs:85-98) for validation
// results: a gate check that returns accept/reject with a reason code.
type LeaseCommitResult struct {
	Accepted bool
	Reason   string // e.g., "missing_active_lease", "request_id_mismatch", "", etc.
}

// LeaseBusy mirrors MainTurnLeaseAcquire::Busy (lib.rs:126-133): fields
// surfaced back to the caller when an in-flight lease already holds this
// identity's key.
type LeaseBusy struct {
	InFlightRequestID  string
	PreviousResponseID string
	WebSocketChainID   *backend.ChainID
}

type LeaseAcquire struct {
	Acquired bool
	Busy     *LeaseBusy
}

type LeaseStore interface {
	Acquire(identity conversation.Identity, reqID string, previousResponseID *string) LeaseAcquire
	Commit(identity conversation.Identity, reqID string, transition LeaseState) error
	Release(identity conversation.Identity, reqID string)
	PromoteWebSocketChain(identity conversation.Identity, reqID string, chainID *backend.ChainID) bool
	ValidateForCommit(identity conversation.Identity, reqID string, websocketChainID *backend.ChainID) LeaseCommitResult
}

type lease struct {
	requestID        string
	previousResponse *string
	websocketChain   *backend.ChainID
	state            LeaseState
}

// InMemoryLeaseStore is the in-memory port of MainTurnLeaseStore
// (lib.rs:85-391). ✱G2 (open gap): an optional persisted-lease variant plus
// a TTL startup sweep is not implemented here; this store is process-local
// only, matching the Rust default.
type InMemoryLeaseStore struct {
	mu     sync.Mutex
	leases map[string]*lease
}

var _ LeaseStore = (*InMemoryLeaseStore)(nil)

func NewInMemoryLeaseStore() *InMemoryLeaseStore {
	return &InMemoryLeaseStore{leases: make(map[string]*lease)}
}

func (s *InMemoryLeaseStore) Acquire(identity conversation.Identity, reqID string, previousResponseID *string) LeaseAcquire {
	key := identity.Key()
	s.mu.Lock()
	defer s.mu.Unlock()
	if existing, ok := s.leases[key]; ok {
		// Dereference previousResponse for LeaseBusy.PreviousResponseID (plain string, not pointer)
		previousResponseValue := ""
		if existing.previousResponse != nil {
			previousResponseValue = *existing.previousResponse
		}
		return LeaseAcquire{
			Acquired: false,
			Busy: &LeaseBusy{
				InFlightRequestID:  existing.requestID,
				PreviousResponseID: previousResponseValue,
				WebSocketChainID:   existing.websocketChain,
			},
		}
	}
	s.leases[key] = &lease{
		requestID:        reqID,
		previousResponse: previousResponseID,
		websocketChain:   nil,
		state:            InFlight,
	}
	return LeaseAcquire{Acquired: true}
}

func (s *InMemoryLeaseStore) Commit(identity conversation.Identity, reqID string, transition LeaseState) error {
	key := identity.Key()
	s.mu.Lock()
	defer s.mu.Unlock()
	existing, ok := s.leases[key]
	if !ok {
		return ErrLeaseMissing
	}
	if existing.requestID != reqID {
		return ErrLeaseRequestMismatch
	}
	if transition == CompletedCommitted && !existing.state.AllowsCommit() {
		return fmt.Errorf("commit rejected: lease state %q does not allow commit", existing.state)
	}
	existing.state = transition
	// Every transition is terminal: Rust's release_with_state (lib.rs:417-424)
	// always follows mark_state with a map removal, for both the commit path
	// and every abort/failure path. A rejected commit leaves the lease
	// untouched so the caller can retry or fall through to an abort/failure
	// transition instead.
	delete(s.leases, key)
	return nil
}

func (s *InMemoryLeaseStore) Release(identity conversation.Identity, reqID string) {
	key := identity.Key()
	s.mu.Lock()
	defer s.mu.Unlock()
	if existing, ok := s.leases[key]; ok && existing.requestID == reqID {
		delete(s.leases, key)
	}
}

// PromoteWebSocketChain ports MainTurnLeaseStore.promote_websocket_chain
// (lib.rs:311-333): set websocket_chain on an existing, still-commit-allowing
// lease. Returns false if the lease is missing, request ID mismatches, or the
// lease state does not allow commit. Returns true immediately if chainID is nil
// (no-op success).
func (s *InMemoryLeaseStore) PromoteWebSocketChain(identity conversation.Identity, reqID string, chainID *backend.ChainID) bool {
	if chainID == nil {
		return true // no-op success
	}
	key := identity.Key()
	s.mu.Lock()
	defer s.mu.Unlock()
	existing, ok := s.leases[key]
	if !ok {
		return false
	}
	if existing.requestID != reqID {
		return false
	}
	if !existing.state.AllowsCommit() {
		return false
	}
	existing.websocketChain = chainID
	return true
}

// ValidateForCommit ports MainTurnLeaseStore::validate_for_commit
// (lib.rs:353-377): read-only validation gate that checks if a lease exists,
// request ID matches, state allows commit, and websocket chain IDs (if present)
// match. Returns Accepted=true only on success paths (both nil, or matching
// non-nil); otherwise returns Accepted=false with a diagnostic reason code.
// This method does NOT mutate the lease.
func (s *InMemoryLeaseStore) ValidateForCommit(identity conversation.Identity, reqID string, websocketChainID *backend.ChainID) LeaseCommitResult {
	key := identity.Key()
	s.mu.Lock()
	defer s.mu.Unlock()

	existing, ok := s.leases[key]
	if !ok {
		return LeaseCommitResult{Accepted: false, Reason: "missing_active_lease"}
	}
	if existing.requestID != reqID {
		return LeaseCommitResult{Accepted: false, Reason: "request_id_mismatch"}
	}
	if !existing.state.AllowsCommit() {
		return LeaseCommitResult{Accepted: false, Reason: string(existing.state)}
	}

	// Match on (existing.websocketChain, websocketChainID):
	// - (Some, Some): must be equal
	// - (Some, None): error "missing_commit_websocket_chain_id"
	// - (None, Some): error "unpromoted_websocket_chain_id"
	// - (None, None): accept
	switch {
	case existing.websocketChain != nil && websocketChainID != nil:
		// Both present: compare
		if *existing.websocketChain == *websocketChainID {
			return LeaseCommitResult{Accepted: true, Reason: ""}
		}
		return LeaseCommitResult{Accepted: false, Reason: "websocket_chain_id_mismatch"}
	case existing.websocketChain != nil && websocketChainID == nil:
		// Lease has chain, but we don't
		return LeaseCommitResult{Accepted: false, Reason: "missing_commit_websocket_chain_id"}
	case existing.websocketChain == nil && websocketChainID != nil:
		// We have chain, but lease doesn't
		return LeaseCommitResult{Accepted: false, Reason: "unpromoted_websocket_chain_id"}
	default:
		// (None, None): both nil, accept
		return LeaseCommitResult{Accepted: true, Reason: ""}
	}
}
