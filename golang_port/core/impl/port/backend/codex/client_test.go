package codex

import (
	"testing"

	"github.com/Bytonomics/cld-gateway/core"
	"github.com/Bytonomics/cld-gateway/core/domain/port/backend"
)

// TestBuildRequestBodyClientMetadata tests the nil vs empty map behavior for
// client_metadata. This regression test ensures that:
// - A non-nil empty map is included in the body as "client_metadata": {}
// - A nil map omits the "client_metadata" key entirely
// This matches Rust's Option<HashMap> semantics (Some(empty) vs None).
func TestBuildRequestBodyClientMetadata(t *testing.T) {
	tests := []struct {
		name                string
		clientMetadata      map[string]string
		shouldHaveMetadata  bool
		expectedMetadataVal map[string]string
		description         string
	}{
		{
			name:                "nil client_metadata omits key",
			clientMetadata:      nil,
			shouldHaveMetadata:  false,
			expectedMetadataVal: nil,
			description:         "nil map should not add client_metadata key to body",
		},
		{
			name:                "non-nil empty client_metadata includes key",
			clientMetadata:      map[string]string{},
			shouldHaveMetadata:  true,
			expectedMetadataVal: map[string]string{},
			description:         "empty map should add client_metadata key with empty map value",
		},
		{
			name:                "non-nil populated client_metadata includes key",
			clientMetadata:      map[string]string{"key1": "value1", "key2": "value2"},
			shouldHaveMetadata:  true,
			expectedMetadataVal: map[string]string{"key1": "value1", "key2": "value2"},
			description:         "populated map should add client_metadata key with all entries",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			req := &backend.Request{
				AccessToken:    core.Secret(""),
				AccountID:      "test-account",
				Model:          "gpt-4",
				Instructions:   "test instruction",
				ClientMetadata: tt.clientMetadata,
			}

			body := buildRequestBody(req)

			// Check if "client_metadata" key is present/absent as expected
			_, hasKey := body["client_metadata"]
			if tt.shouldHaveMetadata && !hasKey {
				t.Errorf("%s: expected client_metadata key to be present, but it was absent", tt.description)
			}
			if !tt.shouldHaveMetadata && hasKey {
				t.Errorf("%s: expected client_metadata key to be absent, but it was present with value %v", tt.description, body["client_metadata"])
			}

			// If key should be present, verify the value matches expected
			if tt.shouldHaveMetadata && hasKey {
				metadataVal, ok := body["client_metadata"].(map[string]string)
				if !ok {
					t.Errorf("%s: expected client_metadata to be map[string]string, got %T", tt.description, body["client_metadata"])
					return
				}

				// Check map length
				if len(metadataVal) != len(tt.expectedMetadataVal) {
					t.Errorf("%s: expected client_metadata to have %d entries, got %d", tt.description, len(tt.expectedMetadataVal), len(metadataVal))
					return
				}

				// Check all key-value pairs match
				for k, v := range tt.expectedMetadataVal {
					if metadataVal[k] != v {
						t.Errorf("%s: expected client_metadata[%q] = %q, got %q", tt.description, k, v, metadataVal[k])
					}
				}
			}
		})
	}
}
