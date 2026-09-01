package config

import (
	"os"
	"path/filepath"
	"testing"
)

func TestLoadProvidersConfigCorrectShape(t *testing.T) {
	tests := []struct {
		name              string
		yaml              string
		expectedActive    string
		expectedModel     string
		expectedUnsupport []string
	}{
		{
			name: "codex backend with complete config",
			yaml: `version: 1
workflow:
  fast_mode: false
providers:
  active: codex
  backends:
    codex:
      default_model: gpt-4-turbo
      unsupported_models:
        - gpt-4-old
        - gpt-3.5-legacy
network:
  listen_addr: 127.0.0.1:6483
`,
			expectedActive:    "codex",
			expectedModel:     "gpt-4-turbo",
			expectedUnsupport: []string{"gpt-4-old", "gpt-3.5-legacy"},
		},
		{
			name: "codex with default unsupported models",
			yaml: `version: 1
workflow:
  fast_mode: false
providers:
  active: codex
  backends:
    codex:
      default_model: gpt-5.6-sol
      unsupported_models:
        - gpt-5.2
        - gpt-5.3-codex
network:
  listen_addr: 127.0.0.1:6483
`,
			expectedActive:    "codex",
			expectedModel:     "gpt-5.6-sol",
			expectedUnsupport: []string{"gpt-5.2", "gpt-5.3-codex"},
		},
		{
			name: "empty unsupported models list",
			yaml: `version: 1
workflow:
  fast_mode: false
providers:
  active: codex
  backends:
    codex:
      default_model: gpt-6.0-latest
      unsupported_models: []
network:
  listen_addr: 127.0.0.1:6483
`,
			expectedActive:    "codex",
			expectedModel:     "gpt-6.0-latest",
			expectedUnsupport: []string{},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// Create temporary YAML file
			tmpDir := t.TempDir()
			configPath := filepath.Join(tmpDir, "test-config.yml")

			if err := os.WriteFile(configPath, []byte(tt.yaml), 0644); err != nil {
				t.Fatalf("failed to write temp config file: %v", err)
			}

			// Load config
			cfg, err := Load(configPath)
			if err != nil {
				t.Fatalf("Load() failed: %v", err)
			}

			// Verify Active field is set correctly
			if cfg.Providers.Active != tt.expectedActive {
				t.Errorf("Providers.Active = %q, want %q", cfg.Providers.Active, tt.expectedActive)
			}

			// Verify Backends map has the expected backend
			backend, exists := cfg.Providers.Backends[tt.expectedActive]
			if !exists {
				t.Fatalf("Providers.Backends[%q] does not exist", tt.expectedActive)
			}

			// Verify DefaultModel from YAML was loaded (not hardcoded Default())
			if backend.DefaultModel != tt.expectedModel {
				t.Errorf("Backends[%q].DefaultModel = %q, want %q", tt.expectedActive, backend.DefaultModel, tt.expectedModel)
			}

			// Verify UnsupportedModels from YAML was loaded
			if len(backend.UnsupportedModels) != len(tt.expectedUnsupport) {
				t.Errorf("Backends[%q].UnsupportedModels length = %d, want %d", tt.expectedActive, len(backend.UnsupportedModels), len(tt.expectedUnsupport))
			}

			for i, model := range backend.UnsupportedModels {
				if i >= len(tt.expectedUnsupport) {
					t.Errorf("Backends[%q].UnsupportedModels has extra element: %q", tt.expectedActive, model)
					break
				}
				if model != tt.expectedUnsupport[i] {
					t.Errorf("Backends[%q].UnsupportedModels[%d] = %q, want %q", tt.expectedActive, i, model, tt.expectedUnsupport[i])
				}
			}
		})
	}
}

func TestLoadDefaultsWhenNoProviderOverride(t *testing.T) {
	// Test that when no providers section is in config file,
	// Default() values are preserved
	yaml := `version: 1
workflow:
  fast_mode: false
network:
  listen_addr: 127.0.0.1:6483
`

	tmpDir := t.TempDir()
	configPath := filepath.Join(tmpDir, "test-config.yml")

	if err := os.WriteFile(configPath, []byte(yaml), 0644); err != nil {
		t.Fatalf("failed to write temp config file: %v", err)
	}

	cfg, err := Load(configPath)
	if err != nil {
		t.Fatalf("Load() failed: %v", err)
	}

	// Should have default active backend
	if cfg.Providers.Active != "codex" {
		t.Errorf("Providers.Active = %q, want %q", cfg.Providers.Active, "codex")
	}

	// Should have default codex backend config
	backend, exists := cfg.Providers.Backends["codex"]
	if !exists {
		t.Fatal("default codex backend not found in Providers.Backends")
	}

	if backend.DefaultModel != DefaultBackendModel {
		t.Errorf("default Backends[codex].DefaultModel = %q, want %q", backend.DefaultModel, DefaultBackendModel)
	}

	if len(backend.UnsupportedModels) != len(DefaultUnsupportedModels) {
		t.Errorf("default Backends[codex].UnsupportedModels length = %d, want %d", len(backend.UnsupportedModels), len(DefaultUnsupportedModels))
	}
}

func TestResolveModelWithLoadedConfig(t *testing.T) {
	// Test that ResolveModel works correctly with loaded config
	tests := []struct {
		name             string
		yaml             string
		requestedModel   string
		expectedSelected string
		expectedReason   string
	}{
		{
			name: "requested model in unsupported list returns default",
			yaml: `version: 1
providers:
  active: codex
  backends:
    codex:
      default_model: gpt-5.6-sol
      unsupported_models:
        - gpt-5.2
        - gpt-5.3-codex
network:
  listen_addr: 127.0.0.1:6483
`,
			requestedModel:   "gpt-5.2",
			expectedSelected: "gpt-5.6-sol",
			expectedReason:   "unsupported_model_compat_override",
		},
		{
			name: "requested model not in unsupported list passes through",
			yaml: `version: 1
providers:
  active: codex
  backends:
    codex:
      default_model: gpt-5.6-sol
      unsupported_models:
        - gpt-5.2
network:
  listen_addr: 127.0.0.1:6483
`,
			requestedModel:   "gpt-4-turbo",
			expectedSelected: "gpt-4-turbo",
			expectedReason:   "passthrough",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			tmpDir := t.TempDir()
			configPath := filepath.Join(tmpDir, "test-config.yml")

			if err := os.WriteFile(configPath, []byte(tt.yaml), 0644); err != nil {
				t.Fatalf("failed to write temp config file: %v", err)
			}

			cfg, err := Load(configPath)
			if err != nil {
				t.Fatalf("Load() failed: %v", err)
			}

			resolution := ResolveModel(cfg, tt.requestedModel)

			if resolution.SelectedBackendModel != tt.expectedSelected {
				t.Errorf("ResolveModel selected = %q, want %q", resolution.SelectedBackendModel, tt.expectedSelected)
			}

			if resolution.SelectionReason != tt.expectedReason {
				t.Errorf("ResolveModel reason = %q, want %q", resolution.SelectionReason, tt.expectedReason)
			}
		})
	}
}
