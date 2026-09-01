package services

import (
	"encoding/json"
	"testing"

	"github.com/Bytonomics/cld-gateway/config"
	"github.com/Bytonomics/cld-gateway/core/domain/dto"
	backendport "github.com/Bytonomics/cld-gateway/core/domain/port/backend"
	stateport "github.com/Bytonomics/cld-gateway/core/domain/port/state"
)

// TestCanonicalValue tests field-by-field reconstruction of message content.
func TestCanonicalValue(t *testing.T) {
	tests := []struct {
		name     string
		messages []dto.Message
		verify   func(t *testing.T, result any)
	}{
		{
			name: "simple text message",
			messages: []dto.Message{
				{
					Role: "user",
					Content: dto.Content{
						Text: strPtr("Hello world"),
					},
				},
			},
			verify: func(t *testing.T, result any) {
				arr, ok := result.([]any)
				if !ok || len(arr) != 1 {
					t.Fatalf("expected array of length 1, got %T", result)
				}
				msg, ok := arr[0].(map[string]any)
				if !ok {
					t.Fatalf("expected message to be map, got %T", arr[0])
				}
				if msg["role"] != "user" {
					t.Errorf("expected role=user, got %v", msg["role"])
				}
				content, ok := msg["content"].([]any)
				if !ok {
					t.Fatalf("expected content to be array, got %T", msg["content"])
				}
				if len(content) != 1 {
					t.Fatalf("expected content array length 1, got %d", len(content))
				}
				block, ok := content[0].(map[string]any)
				if !ok {
					t.Fatalf("expected block to be map, got %T", content[0])
				}
				if block["type"] != "text" {
					t.Errorf("expected type=text, got %v", block["type"])
				}
				if block["text"] != "Hello world" {
					t.Errorf("expected text='Hello world', got %v", block["text"])
				}
			},
		},
		{
			name: "content blocks with multiple types",
			messages: []dto.Message{
				{
					Role: "assistant",
					Content: dto.Content{
						Blocks: []dto.ContentBlock{
							{
								BlockType: "text",
								Text:      strPtr("Assistant response"),
							},
							{
								BlockType: "tool_use",
								ID:        strPtr("tool_123"),
								Name:      strPtr("my_tool"),
								Input:     map[string]any{"key": "value"},
							},
						},
					},
				},
			},
			verify: func(t *testing.T, result any) {
				arr, ok := result.([]any)
				if !ok || len(arr) != 1 {
					t.Fatalf("expected array of length 1")
				}
				msg := arr[0].(map[string]any)
				content := msg["content"].([]any)
				if len(content) != 2 {
					t.Fatalf("expected 2 content blocks, got %d", len(content))
				}

				// First block: text
				block0 := content[0].(map[string]any)
				if block0["type"] != "text" || block0["text"] != "Assistant response" {
					t.Errorf("first block mismatch: %v", block0)
				}

				// Second block: tool_use
				block1 := content[1].(map[string]any)
				if block1["type"] != "tool_use" {
					t.Errorf("second block type mismatch: %v", block1["type"])
				}
				if block1["id"] != "tool_123" {
					t.Errorf("second block id mismatch: %v", block1["id"])
				}
				if block1["name"] != "my_tool" {
					t.Errorf("second block name mismatch: %v", block1["name"])
				}
			},
		},
		{
			name: "cache_control is stripped",
			messages: []dto.Message{
				{
					Role: "user",
					Content: dto.Content{
						Blocks: []dto.ContentBlock{
							{
								BlockType: "text",
								Text:      strPtr("cached content"),
								Extra: map[string]json.RawMessage{
									"cache_control": []byte(`{"type":"ephemeral"}`),
									"other_field":   []byte(`"should_remain"`),
								},
							},
						},
					},
				},
			},
			verify: func(t *testing.T, result any) {
				arr := result.([]any)
				msg := arr[0].(map[string]any)
				content := msg["content"].([]any)
				block := content[0].(map[string]any)

				// cache_control should be stripped
				if _, has := block["cache_control"]; has {
					t.Errorf("cache_control should be stripped")
				}
				// other_field should remain
				if _, has := block["other_field"]; !has {
					t.Errorf("other_field should be present")
				}
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			result := canonicalValue(tt.messages)
			tt.verify(t, result)
		})
	}
}

// TestBranchFingerprints tests the text-extraction and hashing scheme.
func TestBranchFingerprints(t *testing.T) {
	tests := []struct {
		name     string
		messages []dto.Message
		verify   func(t *testing.T, result stateport.BranchFingerprintSet)
	}{
		{
			name: "single user message",
			messages: []dto.Message{
				{
					Role: "user",
					Content: dto.Content{
						Text: strPtr("Hello"),
					},
				},
			},
			verify: func(t *testing.T, result stateport.BranchFingerprintSet) {
				// Should have all three hashes (recent tail, last user, branch state)
				if result.RecentMessageTailHash == nil {
					t.Errorf("RecentMessageTailHash should not be nil")
				}
				if result.LastUserMessageHash == nil {
					t.Errorf("LastUserMessageHash should not be nil")
				}
				if result.BranchStateHash == nil {
					t.Errorf("BranchStateHash should not be nil")
				}
				// For a single message, tail hash and branch state hash should be equal
				if result.RecentMessageTailHash != nil && result.BranchStateHash != nil {
					if *result.RecentMessageTailHash != *result.BranchStateHash {
						t.Errorf("single message: tail and branch hashes should match")
					}
				}
			},
		},
		{
			name: "alternating user and assistant, more than 4 messages",
			messages: []dto.Message{
				{
					Role: "user",
					Content: dto.Content{
						Text: strPtr("First"),
					},
				},
				{
					Role: "assistant",
					Content: dto.Content{
						Text: strPtr("Response 1"),
					},
				},
				{
					Role: "user",
					Content: dto.Content{
						Text: strPtr("Second"),
					},
				},
				{
					Role: "assistant",
					Content: dto.Content{
						Text: strPtr("Response 2"),
					},
				},
				{
					Role: "user",
					Content: dto.Content{
						Text: strPtr("Third"),
					},
				},
			},
			verify: func(t *testing.T, result stateport.BranchFingerprintSet) {
				if result.RecentMessageTailHash == nil {
					t.Errorf("RecentMessageTailHash should not be nil")
				}
				if result.LastUserMessageHash == nil {
					t.Errorf("LastUserMessageHash should not be nil")
				}
				if result.BranchStateHash == nil {
					t.Errorf("BranchStateHash should not be nil")
				}
				// With 5 messages and a 4-message tail window, the tail hash
				// covers a strict subset of the full transcript, so they differ.
				if result.RecentMessageTailHash != nil && result.BranchStateHash != nil {
					if *result.RecentMessageTailHash == *result.BranchStateHash {
						t.Errorf("multi-message: tail and branch hashes should differ")
					}
				}
			},
		},
		{
			name: "empty text messages are skipped",
			messages: []dto.Message{
				{
					Role: "user",
					Content: dto.Content{
						Text: strPtr("   "),
					},
				},
				{
					Role: "user",
					Content: dto.Content{
						Text: strPtr("Real content"),
					},
				},
			},
			verify: func(t *testing.T, result stateport.BranchFingerprintSet) {
				// Only the non-empty message should contribute
				if result.LastUserMessageHash == nil {
					t.Errorf("LastUserMessageHash should not be nil")
				}
			},
		},
		{
			name: "text blocks extracted correctly",
			messages: []dto.Message{
				{
					Role: "user",
					Content: dto.Content{
						Blocks: []dto.ContentBlock{
							{
								BlockType: "text",
								Text:      strPtr("Block 1"),
							},
							{
								BlockType: "text",
								Text:      strPtr("Block 2"),
							},
						},
					},
				},
			},
			verify: func(t *testing.T, result stateport.BranchFingerprintSet) {
				// Text blocks should be joined with double newline
				if result.BranchStateHash == nil {
					t.Errorf("BranchStateHash should not be nil")
				}
			},
		},
		{
			name:     "empty message list",
			messages: []dto.Message{},
			verify: func(t *testing.T, result stateport.BranchFingerprintSet) {
				if result.RecentMessageTailHash != nil {
					t.Errorf("RecentMessageTailHash should be nil for empty messages")
				}
				if result.LastUserMessageHash != nil {
					t.Errorf("LastUserMessageHash should be nil for empty messages")
				}
				if result.BranchStateHash != nil {
					t.Errorf("BranchStateHash should be nil for empty messages")
				}
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			result := branchFingerprints(tt.messages)
			tt.verify(t, result)
		})
	}
}

// TestRequestCompatibilityFingerprint tests that all request components are folded into the hash.
func TestRequestCompatibilityFingerprint(t *testing.T) {
	tests := []struct {
		name        string
		config      *config.Config
		resolution  config.ModelResolution
		backendReq  *backendport.Request
		shouldMatch *backendport.Request // another request; hash should differ if different
	}{
		{
			name: "basic request fingerprint",
			config: &config.Config{
				Providers: config.Providers{
					Active: "codex",
					Backends: map[string]config.BackendProviderConfig{
						"codex": {
							DefaultModel: "claude-3-5-sonnet-20241022",
						},
					},
				},
			},
			resolution: config.ModelResolution{
				SelectedBackendModel: "claude-3-5-sonnet-20241022",
			},
			backendReq: &backendport.Request{
				Model:             "claude-3-5-sonnet-20241022",
				Instructions:      "Be helpful",
				Tools:             []map[string]any{},
				ToolChoice:        "auto",
				ParallelToolCalls: false,
				Text:              nil,
				Reasoning:         nil,
				Include:           []string{},
				ServiceTier:       nil,
			},
			shouldMatch: &backendport.Request{
				Model:             "claude-3-opus-4-20250805",
				Instructions:      "Be unhelpful",
				Tools:             []map[string]any{{"name": "test_tool"}},
				ToolChoice:        "none",
				ParallelToolCalls: true,
				Text:              &map[string]any{"key": "value"},
				Reasoning:         &map[string]any{"type": "enabled"},
				Include:           []string{"thinking"},
				ServiceTier:       strPtr("pro"),
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			hash1 := requestCompatibilityFingerprint(tt.config, tt.resolution, tt.backendReq)
			if hash1 == "" {
				t.Errorf("hash should not be empty")
			}

			// Verify it's a valid hex hash (64 chars for SHA256)
			if len(hash1) != 64 {
				t.Errorf("expected 64-char hex hash, got %d chars", len(hash1))
			}

			// Hashes with different request parameters should differ
			if tt.shouldMatch != nil {
				hash2 := requestCompatibilityFingerprint(tt.config, tt.resolution, tt.shouldMatch)
				if hash1 == hash2 {
					t.Errorf("different requests should produce different hashes")
				}
			}

			// Same request should produce same hash (deterministic)
			hash1Again := requestCompatibilityFingerprint(tt.config, tt.resolution, tt.backendReq)
			if hash1 != hash1Again {
				t.Errorf("same request should produce same hash")
			}
		})
	}
}

// TestExtractMessageText validates text extraction from various message formats.
func TestExtractMessageText(t *testing.T) {
	tests := []struct {
		name     string
		message  dto.Message
		expected string
	}{
		{
			name: "simple text content",
			message: dto.Message{
				Role: "user",
				Content: dto.Content{
					Text: strPtr("Hello world"),
				},
			},
			expected: "Hello world",
		},
		{
			name: "multiple text blocks",
			message: dto.Message{
				Role: "assistant",
				Content: dto.Content{
					Blocks: []dto.ContentBlock{
						{
							BlockType: "text",
							Text:      strPtr("First paragraph"),
						},
						{
							BlockType: "tool_use",
							ID:        strPtr("tool_1"),
							Name:      strPtr("search"),
						},
						{
							BlockType: "text",
							Text:      strPtr("Second paragraph"),
						},
					},
				},
			},
			expected: "First paragraph\n\nSecond paragraph",
		},
		{
			name: "non-text blocks are ignored",
			message: dto.Message{
				Role: "assistant",
				Content: dto.Content{
					Blocks: []dto.ContentBlock{
						{
							BlockType: "tool_use",
							ID:        strPtr("tool_1"),
							Name:      strPtr("search"),
						},
						{
							BlockType: "tool_result",
							ID:        strPtr("tool_1"),
							Content:   "result data",
						},
					},
				},
			},
			expected: "",
		},
		{
			name: "empty text blocks are skipped",
			message: dto.Message{
				Role: "user",
				Content: dto.Content{
					Blocks: []dto.ContentBlock{
						{
							BlockType: "text",
							Text:      strPtr(""),
						},
						{
							BlockType: "text",
							Text:      strPtr("Non-empty"),
						},
					},
				},
			},
			expected: "Non-empty",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			result := extractMessageText(tt.message)
			if result != tt.expected {
				t.Errorf("expected %q, got %q", tt.expected, result)
			}
		})
	}
}
