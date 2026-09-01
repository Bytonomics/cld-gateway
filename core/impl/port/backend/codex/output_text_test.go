package codex

import (
	"encoding/json"
	"testing"
)

// TestExtractTextDeterminism verifies that extracting text from JSON payloads
// with multiple sibling keys is deterministic (always returns the same result
// for the same input), not dependent on Go's non-deterministic map iteration order.
func TestExtractTextDeterminism(t *testing.T) {
	// JSON payload with multiple sibling keys that are not "text", "delta", or "content".
	// Each sibling contains a nested "text" field. Since map iteration order is
	// non-deterministic without sorting, this would previously return different
	// results across runs. With sorting, it should always extract from "zebra"
	// (the last key alphabetically), ensuring deterministic behavior.
	jsonPayload := `{
		"apple": {"text": "from apple"},
		"banana": {"text": "from banana"},
		"cherry": {"text": "from cherry"},
		"zebra": {"text": "from zebra"}
	}`

	// Extract text multiple times from the same payload.
	// All results should be identical, proving deterministic behavior.
	const iterations = 10
	var results []*string

	for i := 0; i < iterations; i++ {
		result := ExtractTextFromData(jsonPayload)
		results = append(results, result)
	}

	// Verify all results are non-nil and identical.
	if len(results) == 0 {
		t.Fatal("expected at least one result")
	}

	for i := 0; i < len(results); i++ {
		if results[i] == nil {
			t.Errorf("iteration %d: result is nil, expected non-nil", i)
			continue
		}
		if i > 0 && *results[i] != *results[0] {
			t.Errorf("iteration %d: got %q, expected %q (determinism failed)",
				i, *results[i], *results[0])
		}
	}

	// The first result should be deterministically "from zebra" because:
	// - Keys are sorted alphabetically: apple, banana, cherry, zebra
	// - extractLastTextFromValue processes them in sorted order
	// - Each sibling overwrites *last with its nested "text" field
	// - The last key processed (zebra) is what remains
	if results[0] != nil && *results[0] != "from zebra" {
		t.Errorf("got %q, expected %q", *results[0], "from zebra")
	}
}

// TestExtractTextNestedSiblings verifies determinism with nested structures
// where multiple sibling branches each contain text fields at different depths.
func TestExtractTextNestedSiblings(t *testing.T) {
	// A more complex payload where multiple nested sibling branches contain
	// text at varying depths. The deterministic sort ensures we process them
	// in a consistent order regardless of map iteration randomization.
	jsonPayload := `{
		"first": {
			"nested": {
				"text": "deep in first"
			}
		},
		"second": {
			"text": "in second"
		},
		"third": {
			"delta": "delta in third"
		}
	}`

	// Call multiple times and verify consistency.
	var results []*string
	for i := 0; i < 5; i++ {
		result := ExtractTextFromData(jsonPayload)
		results = append(results, result)
	}

	// All results should be identical.
	for i := 1; i < len(results); i++ {
		if results[i] == nil && results[0] == nil {
			continue
		}
		if results[i] == nil || results[0] == nil {
			t.Errorf("iteration %d: nil mismatch (one is nil, other is not)", i)
			continue
		}
		if *results[i] != *results[0] {
			t.Errorf("iteration %d: got %q, expected %q",
				i, *results[i], *results[0])
		}
	}
}

// TestExtractTextPreservesSpecialKeys verifies that the "text", "delta", and "content"
// keys are still processed correctly even after the determinism fix.
func TestExtractTextPreservesSpecialKeys(t *testing.T) {
	tests := []struct {
		name     string
		payload  string
		expected string
	}{
		{
			name:     "text field extracted",
			payload:  `{"text": "hello"}`,
			expected: "hello",
		},
		{
			name:     "delta field extracted",
			payload:  `{"delta": "world"}`,
			expected: "world",
		},
		{
			name:     "text takes precedence in order",
			payload:  `{"text": "first", "delta": "second"}`,
			expected: "second", // delta is processed after text, so it overwrites
		},
		{
			name:     "content array handled",
			payload:  `{"content": [{"text": "in array"}]}`,
			expected: "in array",
		},
		{
			name:     "mixed with non-special keys",
			payload:  `{"other": {"text": "from other"}, "text": "direct"}`,
			expected: "from other", // "other" is sorted after "text", so it overwrites
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			result := ExtractTextFromData(tt.payload)
			if result == nil {
				t.Errorf("expected %q, got nil", tt.expected)
				return
			}
			if *result != tt.expected {
				t.Errorf("got %q, expected %q", *result, tt.expected)
			}
		})
	}
}

// TestExtractTextInvalidJSON verifies the fallback behavior when input is not valid JSON.
func TestExtractTextInvalidJSON(t *testing.T) {
	invalidJSON := `{not valid json}`
	result := ExtractTextFromData(invalidJSON)
	if result == nil {
		t.Fatal("expected non-nil result for invalid JSON")
	}
	if *result != invalidJSON {
		t.Errorf("got %q, expected %q (the original string)", *result, invalidJSON)
	}
}

// TestParseOutputItemMessageTextsItemSelection tests the key-selection logic for
// ParseOutputItemMessageTexts, ensuring it correctly prioritizes top-level "item"
// key presence over type, and only falls back to "response.item" when the
// top-level key is genuinely absent.
func TestParseOutputItemMessageTextsItemSelection(t *testing.T) {
	tests := []struct {
		name          string
		eventName     string
		data          string
		shouldSucceed bool
		expectedLen   int
		description   string
	}{
		{
			name:          "top-level item present as object",
			eventName:     "response.output_item.done",
			data:          `{"item":{"type":"output_text","text":"hello world"}}`,
			shouldSucceed: true,
			expectedLen:   1,
			description:   "(a) top-level item present as object → used",
		},
		{
			name:          "top-level item present but null",
			eventName:     "response.output_item.done",
			data:          `{"item":null,"response":{"item":{"type":"output_text","text":"from response"}}}`,
			shouldSucceed: false,
			expectedLen:   0,
			description:   "(b) top-level item present but null → returns nil, no fallback to response.item",
		},
		{
			name:          "top-level item present but string",
			eventName:     "response.output_item.done",
			data:          `{"item":"not an object","response":{"item":{"type":"output_text","text":"from response"}}}`,
			shouldSucceed: false,
			expectedLen:   0,
			description:   "(b) top-level item present but string → returns nil, no fallback to response.item",
		},
		{
			name:          "top-level item present but number",
			eventName:     "response.output_item.done",
			data:          `{"item":42,"response":{"item":{"type":"output_text","text":"from response"}}}`,
			shouldSucceed: false,
			expectedLen:   0,
			description:   "(b) top-level item present but number → returns nil, no fallback to response.item",
		},
		{
			name:          "top-level item absent, response.item present",
			eventName:     "response.output_item.done",
			data:          `{"response":{"item":{"type":"output_text","text":"from response"}}}`,
			shouldSucceed: true,
			expectedLen:   1,
			description:   "(c) top-level item absent, response.item present and valid → falls through correctly",
		},
		{
			name:          "both item and response absent",
			eventName:     "response.output_item.done",
			data:          `{"other_field":"value"}`,
			shouldSucceed: false,
			expectedLen:   0,
			description:   "(d) both absent → nil",
		},
		{
			name:          "response field present but not object",
			eventName:     "response.output_item.done",
			data:          `{"response":"not an object"}`,
			shouldSucceed: false,
			expectedLen:   0,
			description:   "(d) response not an object → nil",
		},
		{
			name:          "wrong event name",
			eventName:     "response.output_item.progress",
			data:          `{"item":{"type":"output_text","text":"hello"}}`,
			shouldSucceed: false,
			expectedLen:   0,
			description:   "wrong event name → nil",
		},
		{
			name:          "top-level item present with message type",
			eventName:     "response.output_item.added",
			data:          `{"item":{"type":"message","content":[{"type":"output_text","text":"msg text"}]}}`,
			shouldSucceed: true,
			expectedLen:   1,
			description:   "(a) top-level item with message type → used",
		},
		{
			name:          "response.item present with message type",
			eventName:     "response.output_item.done",
			data:          `{"response":{"item":{"type":"message","content":[{"type":"output_text","text":"msg text"}]}}}`,
			shouldSucceed: true,
			expectedLen:   1,
			description:   "(c) response.item with message type → falls through correctly",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			result := ParseOutputItemMessageTexts(tt.eventName, tt.data)

			if tt.shouldSucceed {
				if len(result) == 0 {
					t.Errorf("%s: expected text(s), got empty result", tt.description)
				}
				if len(result) != tt.expectedLen {
					t.Errorf("%s: expected %d text(s), got %d", tt.description, tt.expectedLen, len(result))
				}
			} else {
				if len(result) != 0 {
					t.Errorf("%s: expected nil/empty result, got %v", tt.description, result)
				}
			}
		})
	}
}

// BenchmarkExtractTextDeterminism benchmarks the performance of text extraction
// to ensure the sorting fix does not introduce significant overhead.
func BenchmarkExtractTextDeterminism(b *testing.B) {
	jsonPayload := `{
		"apple": {"text": "from apple"},
		"banana": {"text": "from banana"},
		"cherry": {"text": "from cherry"},
		"zebra": {"text": "from zebra"}
	}`

	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		ExtractTextFromData(jsonPayload)
	}
}

// TestExtractTextManyKeys tests determinism with a large number of sibling keys
// to verify the sort-based fix scales well.
func TestExtractTextManyKeys(t *testing.T) {
	// Build a JSON object with many sibling keys, each with a "text" field.
	obj := make(map[string]interface{})
	for i := 0; i < 100; i++ {
		key := string(rune('a' + (i % 26)))
		// Create keys like a0, a1, ..., z99 to get varied sorting order
		key = key + string(rune('0'+(i/26)))
		obj[key] = map[string]interface{}{"text": "text from " + key}
	}

	data, err := json.Marshal(obj)
	if err != nil {
		t.Fatalf("failed to marshal test object: %v", err)
	}

	// Extract multiple times and verify consistency.
	var results []*string
	for i := 0; i < 5; i++ {
		result := ExtractTextFromData(string(data))
		results = append(results, result)
	}

	// All results should be identical.
	for i := 1; i < len(results); i++ {
		if results[i] == nil && results[0] == nil {
			continue
		}
		if results[i] == nil || results[0] == nil {
			t.Errorf("iteration %d: nil mismatch", i)
			continue
		}
		if *results[i] != *results[0] {
			t.Errorf("iteration %d: got %q, expected %q",
				i, *results[i], *results[0])
		}
	}
}
