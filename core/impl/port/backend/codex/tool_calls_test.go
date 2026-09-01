package codex

import (
	"testing"
)

// TestParseOutputItemToolCallItemSelection tests the key-selection logic for
// ParseOutputItemToolCall, ensuring it correctly prioritizes top-level "item"
// key presence over type, and only falls back to "response.item" when the
// top-level key is genuinely absent.
func TestParseOutputItemToolCallItemSelection(t *testing.T) {
	tests := []struct {
		name          string
		eventName     string
		data          string
		shouldSucceed bool
		description   string
	}{
		{
			name:          "top-level item present as function_call",
			eventName:     "response.output_item.done",
			data:          `{"item":{"type":"function_call","call_id":"call123","name":"test_func","arguments":"{}"}}`,
			shouldSucceed: true,
			description:   "(a) top-level item present as object → used",
		},
		{
			name:          "top-level item present but null",
			eventName:     "response.output_item.done",
			data:          `{"item":null,"response":{"item":{"type":"function_call","call_id":"call456","name":"fallback_func","arguments":"{}"}}}`,
			shouldSucceed: false,
			description:   "(b) top-level item present but null → returns nil, no fallback to response.item",
		},
		{
			name:          "top-level item present but string",
			eventName:     "response.output_item.done",
			data:          `{"item":"not an object","response":{"item":{"type":"function_call","call_id":"call789","name":"fallback_func","arguments":"{}"}}}`,
			shouldSucceed: false,
			description:   "(b) top-level item present but string → returns nil, no fallback to response.item",
		},
		{
			name:          "top-level item present but number",
			eventName:     "response.output_item.done",
			data:          `{"item":42,"response":{"item":{"type":"function_call","call_id":"call999","name":"fallback_func","arguments":"{}"}}}`,
			shouldSucceed: false,
			description:   "(b) top-level item present but number → returns nil, no fallback to response.item",
		},
		{
			name:          "top-level item absent, response.item present",
			eventName:     "response.output_item.done",
			data:          `{"response":{"item":{"type":"function_call","call_id":"call555","name":"response_func","arguments":"{}"}}}`,
			shouldSucceed: true,
			description:   "(c) top-level item absent, response.item present and valid → falls through correctly",
		},
		{
			name:          "both item and response absent",
			eventName:     "response.output_item.done",
			data:          `{"other_field":"value"}`,
			shouldSucceed: false,
			description:   "(d) both absent → nil",
		},
		{
			name:          "response field present but not object",
			eventName:     "response.output_item.done",
			data:          `{"response":"not an object"}`,
			shouldSucceed: false,
			description:   "(d) response not an object → nil",
		},
		{
			name:          "wrong event name with valid item",
			eventName:     "response.output_item.progress",
			data:          `{"item":{"type":"function_call","call_id":"call111","name":"test_func","arguments":"{}"}}`,
			shouldSucceed: false,
			description:   "wrong event name → nil",
		},
		{
			name:          "top-level item present with custom_tool_call",
			eventName:     "response.output_item.added",
			data:          `{"item":{"type":"custom_tool_call","call_id":"custom123","name":"my_tool","input":"{\"arg\":\"value\"}"}}`,
			shouldSucceed: true,
			description:   "(a) top-level item with custom_tool_call → used",
		},
		{
			name:          "response.item present with tool_search_call",
			eventName:     "response.output_item.done",
			data:          `{"response":{"item":{"type":"tool_search_call","call_id":"ts789","arguments":"{\"query\":\"search term\"}"}}}`,
			shouldSucceed: true,
			description:   "(c) response.item with tool_search_call → falls through correctly",
		},
		{
			name:          "top-level item present with local_shell_call",
			eventName:     "response.output_item.added",
			data:          `{"item":{"type":"local_shell_call","call_id":"shell123","status":"ready"}}`,
			shouldSucceed: true,
			description:   "(a) top-level item with local_shell_call → used",
		},
		{
			name:          "top-level item present but wrong type (array)",
			eventName:     "response.output_item.done",
			data:          `{"item":[],"response":{"item":{"type":"function_call","call_id":"call999","name":"fallback","arguments":"{}"}}}`,
			shouldSucceed: false,
			description:   "(b) top-level item present but array → returns nil, no fallback",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			result := ParseOutputItemToolCall(tt.eventName, tt.data)

			if tt.shouldSucceed {
				if result == nil {
					t.Errorf("%s: expected ToolCall, got nil", tt.description)
				}
			} else {
				if result != nil {
					t.Errorf("%s: expected nil, got %+v", tt.description, result)
				}
			}
		})
	}
}

// TestParseOutputItemToolCallInvalidJSON tests that invalid JSON returns nil.
func TestParseOutputItemToolCallInvalidJSON(t *testing.T) {
	result := ParseOutputItemToolCall("response.output_item.done", `{invalid json}`)
	if result != nil {
		t.Errorf("expected nil for invalid JSON, got %+v", result)
	}
}

// TestParseOutputItemToolCallBadToolType tests that unknown tool types return nil.
func TestParseOutputItemToolCallBadToolType(t *testing.T) {
	result := ParseOutputItemToolCall("response.output_item.done",
		`{"item":{"type":"unknown_call","call_id":"c1"}}`)
	if result != nil {
		t.Errorf("expected nil for unknown tool type, got %+v", result)
	}
}
