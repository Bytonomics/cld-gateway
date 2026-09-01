package backend

import (
	"encoding/json"
	"testing"

	"github.com/Bytonomics/cld-gateway/core"
)

func TestTerminalEvents(t *testing.T) {
	expected := []string{
		"response.completed",
		"response.failed",
		"response.cancelled",
		"error",
	}
	if len(TerminalEvents) != len(expected) {
		t.Errorf("TerminalEvents length = %d, want %d", len(TerminalEvents), len(expected))
	}
	for i, ev := range TerminalEvents {
		if ev != expected[i] {
			t.Errorf("TerminalEvents[%d] = %s, want %s", i, ev, expected[i])
		}
	}
}

// BuildTestRequestJSON creates a JSON body from a Request for testing purposes
func BuildTestRequestJSON(req *Request) ([]byte, error) {
	// Create a shallow copy without AccessToken for JSON serialization
	testReq := *req
	return json.Marshal(testReq)
}

// ExampleRequestBuilder helps construct test Request instances
type ExampleRequestBuilder struct {
	request Request
}

func NewExampleRequestBuilder() *ExampleRequestBuilder {
	return &ExampleRequestBuilder{
		request: Request{
			Model:             "test-model",
			Instructions:      "test instructions",
			Input:             []map[string]any{{"role": "user", "content": "test"}},
			Tools:             []map[string]any{},
			ToolChoice:        "auto",
			ParallelToolCalls: true,
			Stream:            true,
			Include:           []string{"all"},
			ClientMetadata:    map[string]string{"test-key": "test-value"},
		},
	}
}

func (b *ExampleRequestBuilder) WithAccessToken(token core.Secret) *ExampleRequestBuilder {
	b.request.AccessToken = token
	return b
}

func (b *ExampleRequestBuilder) WithAccountID(accountID string) *ExampleRequestBuilder {
	b.request.AccountID = accountID
	return b
}

func (b *ExampleRequestBuilder) WithModel(model string) *ExampleRequestBuilder {
	b.request.Model = model
	return b
}

func (b *ExampleRequestBuilder) Build() *Request {
	return &b.request
}
