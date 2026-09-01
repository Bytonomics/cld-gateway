package config

import (
	"errors"
	"os"
	"path/filepath"
	"strings"

	"github.com/spf13/viper"
)

const (
	FASTServiceTier     = "priority"
	DefaultBackendModel = "gpt-5.6-sol"
)

var DefaultUnsupportedModels = []string{"gpt-5.2", "gpt-5.3-codex"}

type Config struct {
	Version   uint           `mapstructure:"version"`
	Workflow  WorkflowConfig `mapstructure:"workflow"`
	Providers Providers      `mapstructure:"providers"`
	Network   NetworkConfig  `mapstructure:"network"`
}

type WorkflowConfig struct {
	FastMode          bool                     `mapstructure:"fast_mode"`
	ContextManagement ContextManagementConfig  `mapstructure:"context_management"`
	ClaudeCode        ClaudeCodeWorkflowConfig `mapstructure:"claude_code"`
	ConversationState ConversationStateConfig  `mapstructure:"conversation_state"`
}

type ContextManagementConfig struct {
	Enabled       bool                        `mapstructure:"enabled"`
	Mode          string                      `mapstructure:"mode"`
	DefaultEdits  []map[string]any            `mapstructure:"default_edits"`
	OverrideEdits *[]map[string]any           `mapstructure:"override_edits"`
	HardLimits    ContextManagementHardLimits `mapstructure:"hard_limits"`
}

type ContextManagementHardLimits struct {
	MaxToolResultChars     *int `mapstructure:"max_tool_result_chars"`
	MaxToolUsesToKeep      *int `mapstructure:"max_tool_uses_to_keep"`
	MaxThinkingTurnsToKeep *int `mapstructure:"max_thinking_turns_to_keep"`
}

type ClaudeCodeWorkflowConfig struct {
	SlashCommands ClaudeCodeSlashCommandConfig `mapstructure:"slash_commands"`
}

type ClaudeCodeSlashCommandConfig struct {
	Enabled bool   `mapstructure:"enabled"`
	Mode    string `mapstructure:"mode"`
}

type ConversationStateConfig struct {
	Enabled          bool                             `mapstructure:"enabled"`
	PersistenceRoot  *string                          `mapstructure:"persistence_root"`
	CorruptionPolicy string                           `mapstructure:"corruption_policy"`
	Retention        ConversationStateRetentionConfig `mapstructure:"retention"`
}

type ConversationStateRetentionConfig struct {
	MaxSessionAgeDays *uint64 `mapstructure:"max_session_age_days"`
}

type NetworkConfig struct {
	ListenAddr   string   `mapstructure:"listen_addr"`
	AllowedHosts []string `mapstructure:"allowed_hosts"`
}

// Providers is our OVERRIDE: map keyed by backend name
type Providers struct {
	Backends map[string]BackendProviderConfig `mapstructure:",inline"`
	Active   string                           `mapstructure:"active"`
}

type BackendProviderConfig struct {
	Active            bool     `mapstructure:"active"`
	DefaultModel      string   `mapstructure:"default_model"`
	UnsupportedModels []string `mapstructure:"unsupported_models"`
}

type ModelResolution struct {
	Requested            string
	SelectedBackendModel string
	SelectionReason      string
}

// Default returns a Config with all defaults set
func Default() *Config {
	return &Config{
		Version: 1,
		Workflow: WorkflowConfig{
			FastMode: false,
			ContextManagement: ContextManagementConfig{
				Enabled: true,
				Mode:    "follow_request",
			},
			ClaudeCode: ClaudeCodeWorkflowConfig{
				SlashCommands: ClaudeCodeSlashCommandConfig{
					Enabled: true,
					Mode:    "promote_latest",
				},
			},
			ConversationState: ConversationStateConfig{
				Enabled:          true,
				CorruptionPolicy: "fail_closed",
			},
		},
		Providers: Providers{
			Backends: map[string]BackendProviderConfig{
				"codex": {
					Active:            true,
					DefaultModel:      DefaultBackendModel,
					UnsupportedModels: DefaultUnsupportedModels,
				},
			},
			Active: "codex",
		},
		Network: NetworkConfig{
			ListenAddr: "127.0.0.1:6483",
		},
	}
}

// ResolveModel ports Rust resolve_model logic (config.rs:269-288)
// Reason strings: "unsupported_model_compat_override", "passthrough"
func ResolveModel(cfg *Config, requested string) ModelResolution {
	activeBackendName := cfg.Providers.Active
	activeBackend, exists := cfg.Providers.Backends[activeBackendName]
	if !exists {
		// Fallback: if map has single entry, use that; otherwise use defaults
		if len(cfg.Providers.Backends) == 1 {
			for _, v := range cfg.Providers.Backends {
				activeBackend = v
				break
			}
		} else {
			// Map empty: use default model
			activeBackend = BackendProviderConfig{
				DefaultModel:      DefaultBackendModel,
				UnsupportedModels: DefaultUnsupportedModels,
			}
		}
	}

	// Check if requested is in unsupported list
	for _, unsupported := range activeBackend.UnsupportedModels {
		if requested == unsupported {
			return ModelResolution{
				Requested:            requested,
				SelectedBackendModel: activeBackend.DefaultModel,
				SelectionReason:      "unsupported_model_compat_override",
			}
		}
	}

	return ModelResolution{
		Requested:            requested,
		SelectedBackendModel: requested,
		SelectionReason:      "passthrough",
	}
}

// ServiceTier returns "priority" if fast_mode enabled, else nil
func ServiceTier(cfg *Config) *string {
	if cfg.Workflow.FastMode {
		tier := FASTServiceTier
		return &tier
	}
	return nil
}

// DefaultPath ports the Rust gateway_config_path_from_sources resolution
// order (crates/gateway-core/src/config.rs:220-235): GATEWAY_CONFIG_PATH
// (full path) wins; else GATEWAY_HOME/config-dev.yml; else
// ~/.gateway/config-dev.yml.
func DefaultPath() string {
	if path := os.Getenv("GATEWAY_CONFIG_PATH"); path != "" {
		return path
	}

	if home := os.Getenv("GATEWAY_HOME"); home != "" {
		return filepath.Join(home, "config-dev.yml")
	}

	homeDir, err := os.UserHomeDir()
	if err != nil {
		homeDir = "."
	}

	return filepath.Join(homeDir, ".gateway", "config-dev.yml")
}

// Load reads YAML config at path via viper, merging it onto Default() for
// any field left unset, applying GATEWAY_-prefixed env var overrides. A
// missing file is not an error: it mirrors Rust's load_gateway_config,
// which returns GatewayConfig::default() when the file does not exist.
func Load(path string) (*Config, error) {
	cfg := Default()

	v := viper.New()
	v.SetConfigFile(path)
	v.SetConfigType("yaml")
	v.SetEnvPrefix("GATEWAY")
	v.SetEnvKeyReplacer(strings.NewReplacer(".", "_"))
	v.AutomaticEnv()

	if err := v.ReadInConfig(); err != nil {
		var notFoundErr viper.ConfigFileNotFoundError
		if errors.As(err, &notFoundErr) || os.IsNotExist(err) {
			return cfg, nil
		}
		return nil, err
	}

	if err := v.Unmarshal(cfg); err != nil {
		return nil, err
	}

	return cfg, nil
}
