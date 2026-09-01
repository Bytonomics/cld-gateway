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
	Acquire(identity conversation.Identity, reqID string) LeaseAcquire
	Commit(identity conversation.Identity, reqID string, transition LeaseState) error
	Release(identity conversation.Identity, reqID string)
}

type lease struct {
	requestID string
	state     LeaseState
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

func (s *InMemoryLeaseStore) Acquire(identity conversation.Identity, reqID string) LeaseAcquire {
	key := identity.Key()
	s.mu.Lock()
	defer s.mu.Unlock()
	if existing, ok := s.leases[key]; ok {
		return LeaseAcquire{
			Acquired: false,
			Busy: &LeaseBusy{
				InFlightRequestID: existing.requestID,
			},
		}
	}
	s.leases[key] = &lease{requestID: reqID, state: InFlight}
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
