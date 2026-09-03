package services

import (
	"context"
	"errors"
	"testing"

	"github.com/Bytonomics/cld-gateway/core/domain/conversation"
	backendport "github.com/Bytonomics/cld-gateway/core/domain/port/backend"
	stateport "github.com/Bytonomics/cld-gateway/core/domain/port/state"
	"github.com/Bytonomics/cld-gateway/core/domain/transport"
)

// fakeSelector implements transport.Selector for testing.
type fakeSelector struct {
	decision transport.Decision
	err      error
}

func (f *fakeSelector) Select(ctx context.Context, plan transport.Plan) (transport.Decision, error) {
	return f.decision, f.err
}

// fakeBackendForSelectTransport implements backendport.Backend for testing selectTransport.
type fakeBackendForSelectTransport struct{}

func (f *fakeBackendForSelectTransport) SendUnary(ctx context.Context, req *backendport.Request) (*backendport.Response, error) {
	return nil, nil
}

func (f *fakeBackendForSelectTransport) SendStream(ctx context.Context, req *backendport.Request) (<-chan backendport.Event, error) {
	return nil, nil
}

func (f *fakeBackendForSelectTransport) Capabilities() backendport.Capabilities {
	return backendport.Capabilities{}
}

func (f *fakeBackendForSelectTransport) EvictSession(key backendport.SessionKey) {}

func (f *fakeBackendForSelectTransport) HasLiveSession(key backendport.SessionKey) bool {
	return false
}

func (f *fakeBackendForSelectTransport) LiveChainID(key backendport.SessionKey) (backendport.ChainID, bool) {
	return "", false
}

// TestSelectTransport_SelectorError_RecordsWarning tests that a Selector error
// results in a "delta_calculation_failed" warning being appended to plan.warnings.
func TestSelectTransport_SelectorError_RecordsWarning(t *testing.T) {
	s := New(Deps{
		Selector: &fakeSelector{
			decision: transport.Decision{},
			err:      errors.New("selector failed"),
		},
		Backend: &fakeBackendForSelectTransport{},
	})

	plan := &turnPlan{
		identity:   conversation.Identity{},
		backendReq: &backendport.Request{},
	}

	selection := stateport.BranchSelectionResult{}

	s.selectTransport(context.Background(), plan, selection)

	if len(plan.warnings) != 1 {
		t.Fatalf("expected 1 warning, got %d", len(plan.warnings))
	}
	if plan.warnings[0].Code != "delta_calculation_failed" {
		t.Errorf("expected warning code 'delta_calculation_failed', got %q", plan.warnings[0].Code)
	}
}

// TestSelectTransport_CheckpointExistsButNotUsed_RecordsWarning tests that when
// a checkpoint exists but UseWS is false, a "delta_calculation_skipped" warning
// is appended to plan.warnings.
func TestSelectTransport_CheckpointExistsButNotUsed_RecordsWarning(t *testing.T) {
	s := New(Deps{
		Selector: &fakeSelector{
			decision: transport.Decision{UseWS: false},
			err:      nil,
		},
		Backend: &fakeBackendForSelectTransport{},
	})

	plan := &turnPlan{
		identity:   conversation.Identity{},
		backendReq: &backendport.Request{},
	}

	selection := stateport.BranchSelectionResult{
		Branch: stateport.BranchMetadata{
			OpenAiCheckpoint: &stateport.OpenAiCheckpoint{
				ResponseID: "resp_123",
			},
		},
	}

	s.selectTransport(context.Background(), plan, selection)

	if len(plan.warnings) != 1 {
		t.Fatalf("expected 1 warning, got %d", len(plan.warnings))
	}
	if plan.warnings[0].Code != "delta_calculation_skipped" {
		t.Errorf("expected warning code 'delta_calculation_skipped', got %q", plan.warnings[0].Code)
	}
}

// TestSelectTransport_NoCheckpointNeverExisted_NoWarning tests that when there
// is no checkpoint and UseWS is false, no warning is recorded.
func TestSelectTransport_NoCheckpointNeverExisted_NoWarning(t *testing.T) {
	s := New(Deps{
		Selector: &fakeSelector{
			decision: transport.Decision{UseWS: false},
			err:      nil,
		},
		Backend: &fakeBackendForSelectTransport{},
	})

	plan := &turnPlan{
		identity:   conversation.Identity{},
		backendReq: &backendport.Request{},
	}

	selection := stateport.BranchSelectionResult{}

	s.selectTransport(context.Background(), plan, selection)

	if len(plan.warnings) != 0 {
		t.Fatalf("expected 0 warnings, got %d", len(plan.warnings))
	}
}

// TestSelectTransport_UseWS_NoWarning tests that when UseWS is true, no warning
// is recorded and plan.backendReq.PreviousResponseID is set.
func TestSelectTransport_UseWS_NoWarning(t *testing.T) {
	s := New(Deps{
		Selector: &fakeSelector{
			decision: transport.Decision{UseWS: true},
			err:      nil,
		},
		Backend: &fakeBackendForSelectTransport{},
	})

	plan := &turnPlan{
		identity:   conversation.Identity{},
		backendReq: &backendport.Request{},
	}

	selection := stateport.BranchSelectionResult{
		Branch: stateport.BranchMetadata{
			OpenAiCheckpoint: &stateport.OpenAiCheckpoint{
				ResponseID: "resp_123",
			},
		},
	}

	s.selectTransport(context.Background(), plan, selection)

	if len(plan.warnings) != 0 {
		t.Fatalf("expected 0 warnings, got %d", len(plan.warnings))
	}
	if plan.backendReq.PreviousResponseID == nil {
		t.Errorf("expected plan.backendReq.PreviousResponseID to be set, got nil")
	}
	if plan.backendReq.PreviousResponseID != nil && *plan.backendReq.PreviousResponseID != "resp_123" {
		t.Errorf("expected PreviousResponseID='resp_123', got %q", *plan.backendReq.PreviousResponseID)
	}
}
