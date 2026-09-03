package errors

import (
	"fmt"
	"testing"

	"github.com/Bytonomics/cld-gateway/core/domain/port/backend"
)

type fakeUpstreamError struct {
	status int
	body   string
}

func (e *fakeUpstreamError) Error() string        { return "upstream error" }
func (e *fakeUpstreamError) UpstreamStatus() int  { return e.status }
func (e *fakeUpstreamError) UpstreamBody() string { return e.body }

var _ backend.UpstreamStatusError = (*fakeUpstreamError)(nil)

func TestClassify_PlainInternalError_SuggestsIssue(t *testing.T) {
	gwErr := Classify(fmt.Errorf("some internal failure"))
	if gwErr.Origin != OriginInternal {
		t.Errorf("Origin = %q, want %q", gwErr.Origin, OriginInternal)
	}
	if !gwErr.SuggestIssue {
		t.Error("SuggestIssue = false, want true")
	}
	if gwErr.Instruction == "" {
		t.Error("Instruction is empty, want non-empty")
	}
	wantPrefix := "[CLD-Gateway] "
	if len(gwErr.Message) < len(wantPrefix) || gwErr.Message[:len(wantPrefix)] != wantPrefix {
		t.Errorf("Message = %q, want prefix %q", gwErr.Message, wantPrefix)
	}
	if gwErr.Code != CodeAPI {
		t.Errorf("Code = %q, want %q", gwErr.Code, CodeAPI)
	}
	if gwErr.HTTPStatus != 500 {
		t.Errorf("HTTPStatus = %d, want 500", gwErr.HTTPStatus)
	}
}

func TestClassify_ExistingAppError_PreservesCodeAndStatus(t *testing.T) {
	gwErr := Classify(New(CodeInvalidRequest, "bad field X", 400))
	if gwErr.Origin != OriginInternal {
		t.Errorf("Origin = %q, want %q", gwErr.Origin, OriginInternal)
	}
	if !gwErr.SuggestIssue {
		t.Error("SuggestIssue = false, want true (gateway-originated 4xx should default to possible-bug)")
	}
	if gwErr.Code != CodeInvalidRequest {
		t.Errorf("Code = %q, want %q", gwErr.Code, CodeInvalidRequest)
	}
	if gwErr.HTTPStatus != 400 {
		t.Errorf("HTTPStatus = %d, want 400", gwErr.HTTPStatus)
	}
}

func TestClassify_UpstreamQuota429_SuppressesIssue(t *testing.T) {
	fakeErr := &fakeUpstreamError{status: 429, body: "rate limited"}
	wrapped := Wrap(fakeErr, CodeRateLimit, "backend request failed", 502)
	gwErr := Classify(wrapped)
	if gwErr.Origin != OriginUpstream {
		t.Errorf("Origin = %q, want %q", gwErr.Origin, OriginUpstream)
	}
	if gwErr.SuggestIssue {
		t.Error("SuggestIssue = true, want false for a 429 quota rejection")
	}
	if gwErr.Instruction == "" {
		t.Error("Instruction is empty, want a non-empty explanation that this is not a gateway bug")
	}
}

func TestClassify_UpstreamQuotaBodyMarker_SuppressesIssue(t *testing.T) {
	fakeErr := &fakeUpstreamError{status: 400, body: "insufficient_quota: your account has no credits"}
	wrapped := Wrap(fakeErr, CodeAPI, "backend request failed", 502)
	gwErr := Classify(wrapped)
	if gwErr.Origin != OriginUpstream {
		t.Errorf("Origin = %q, want %q", gwErr.Origin, OriginUpstream)
	}
	if gwErr.SuggestIssue {
		t.Error("SuggestIssue = true, want false for a quota-body-marker 4xx")
	}
}

func TestClassify_UpstreamNonQuota5xx_SuggestsIssue(t *testing.T) {
	fakeErr := &fakeUpstreamError{status: 502, body: "internal server error"}
	wrapped := Wrap(fakeErr, CodeAPI, "backend request failed", 502)
	gwErr := Classify(wrapped)
	if gwErr.Origin != OriginUpstream {
		t.Errorf("Origin = %q, want %q", gwErr.Origin, OriginUpstream)
	}
	if !gwErr.SuggestIssue {
		t.Error("SuggestIssue = false, want true for a genuine upstream 5xx")
	}
	if gwErr.Instruction == "" {
		t.Error("Instruction is empty, want non-empty")
	}
}

func TestClassify_UpstreamNonQuota4xx_SuggestsIssue(t *testing.T) {
	fakeErr := &fakeUpstreamError{status: 400, body: "invalid model id"}
	wrapped := Wrap(fakeErr, CodeAPI, "backend request failed", 502)
	gwErr := Classify(wrapped)
	if gwErr.Origin != OriginUpstream {
		t.Errorf("Origin = %q, want %q", gwErr.Origin, OriginUpstream)
	}
	if !gwErr.SuggestIssue {
		t.Error("SuggestIssue = false, want true for a non-quota, non-429, non-503 upstream 4xx - could be a gateway-caused defect")
	}
	if gwErr.Instruction == "" {
		t.Error("Instruction is empty, want non-empty")
	}
}

func TestClassify_UpstreamServiceUnavailable503_SuppressesIssue(t *testing.T) {
	fakeErr := &fakeUpstreamError{status: 503, body: "service unavailable"}
	wrapped := Wrap(fakeErr, CodeAPI, "backend request failed", 502)
	gwErr := Classify(wrapped)
	if gwErr.Origin != OriginUpstream {
		t.Errorf("Origin = %q, want %q", gwErr.Origin, OriginUpstream)
	}
	if gwErr.SuggestIssue {
		t.Error("SuggestIssue = true, want false for a 503 service-unavailable rejection")
	}
	if gwErr.Instruction == "" {
		t.Error("Instruction is empty, want a non-empty explanation that this is not a gateway bug")
	}
}

func TestClassify_WithProviderAndModel_IncludesBothInMessage(t *testing.T) {
	appErr := New(CodeAPI, "backend request failed", 500)
	appErr.Provider = "codex"
	appErr.Model = "gpt-5.1"
	gwErr := Classify(appErr)
	want := "[CLD-Gateway] backend request failed (provider: codex, model: gpt-5.1)"
	if gwErr.Message != want {
		t.Errorf("Message = %q, want %q", gwErr.Message, want)
	}
}

func TestClassify_SuppressedUpstream_MessageNamesProviderAndModel(t *testing.T) {
	fakeErr := &fakeUpstreamError{status: 429, body: "rate limited"}
	wrapped := Wrap(fakeErr, CodeRateLimit, "backend request failed", 502)
	wrapped.Provider = "codex"
	wrapped.Model = "gpt-5.1"
	gwErr := Classify(wrapped)
	if gwErr.SuggestIssue {
		t.Error("SuggestIssue = true, want false for a 429 quota rejection")
	}
	wantMsg := "[CLD-Gateway] backend request failed (provider: codex, model: gpt-5.1)"
	if gwErr.Message != wantMsg {
		t.Errorf("Message = %q, want %q", gwErr.Message, wantMsg)
	}
	wantInstruction := "Neither Claude Code nor cld-gateway can do anything about this (provider: codex, model: gpt-5.1) - it's on the upstream provider's side."
	if gwErr.Instruction != wantInstruction {
		t.Errorf("Instruction = %q, want %q", gwErr.Instruction, wantInstruction)
	}
}
