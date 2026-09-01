package services

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"

	"github.com/Bytonomics/cld-gateway/core/domain/dto"
	apperr "github.com/Bytonomics/cld-gateway/core/domain/errors"
	"github.com/Bytonomics/cld-gateway/core/domain/services"
)

// modelEnvKeys names the four env-var keys model_catalog_from_settings
// reads per fallback model slot (lib.rs:659-718).
type modelEnvKeys struct {
	id, name, description, maxInputTokens string
}

// fallbackModelSlots ports the five add_model_from_env call sites
// (lib.rs:659-718): id/name/description/max-input-tokens env-var keys plus
// the default name/description used when the client settings JSON has no
// explicit "models" array.
var fallbackModelSlots = []struct {
	keys                      modelEnvKeys
	defaultName, defaultDescr string
}{
	{modelEnvKeys{"ANTHROPIC_DEFAULT_HAIKU_MODEL", "ANTHROPIC_DEFAULT_HAIKU_MODEL_NAME", "ANTHROPIC_DEFAULT_HAIKU_MODEL_DESCRIPTION", "ANTHROPIC_DEFAULT_HAIKU_MAX_TOKENS"}, "GPT-5.4 Mini", "OpenAI small/fallback model"},
	{modelEnvKeys{"ANTHROPIC_DEFAULT_SONNET_MODEL", "ANTHROPIC_DEFAULT_SONNET_MODEL_NAME", "ANTHROPIC_DEFAULT_SONNET_MODEL_DESCRIPTION", "ANTHROPIC_DEFAULT_SONNET_MAX_TOKENS"}, "GPT-5.4", "OpenAI general-purpose model"},
	{modelEnvKeys{"ANTHROPIC_DEFAULT_OPUS_MODEL", "ANTHROPIC_DEFAULT_OPUS_MODEL_NAME", "ANTHROPIC_DEFAULT_OPUS_MODEL_DESCRIPTION", "ANTHROPIC_DEFAULT_OPUS_MAX_TOKENS"}, "GPT-5.5", "OpenAI reasoning model"},
	{modelEnvKeys{"ANTHROPIC_DEFAULT_FABLE_MODEL", "ANTHROPIC_DEFAULT_FABLE_MODEL_NAME", "ANTHROPIC_DEFAULT_FABLE_MODEL_DESCRIPTION", "ANTHROPIC_DEFAULT_FABLE_MAX_TOKENS"}, "GPT-5.5 Pro", "OpenAI highest-capability model"},
	{modelEnvKeys{"ANTHROPIC_CUSTOM_MODEL_OPTION", "ANTHROPIC_CUSTOM_MODEL_OPTION_NAME", "ANTHROPIC_CUSTOM_MODEL_OPTION_DESCRIPTION", "ANTHROPIC_CUSTOM_MODEL_OPTION_MAX_TOKENS"}, "Custom model", "Custom model option"},
}

// claudeGatewaySettings ports ClaudeGatewaySettings (lib.rs:486-491).
type claudeGatewaySettings struct {
	Models []claudeGatewayModel `json:"models"`
	Env    map[string]any       `json:"env"`
}

// claudeGatewayModel ports ClaudeGatewayModel (lib.rs:493-502).
type claudeGatewayModel struct {
	ID             string  `json:"id"`
	Name           *string `json:"name,omitempty"`
	Description    *string `json:"description,omitempty"`
	MaxInputTokens *uint64 `json:"max_input_tokens,omitempty"`
}

// ModelsService implements services.ModelsService, porting
// v1_models_with_state / model_catalog_from_settings
// (crates/gateway-http-anthropic/src/lib.rs:922-960, 636-721): read the
// local Claude gateway settings JSON and build the /v1/models catalog from
// its explicit "models" array, or - when that array is empty - from the
// ANTHROPIC_DEFAULT_*_MODEL family of env entries in that same file.
type ModelsService struct {
	settingsPath string
}

var _ services.ModelsService = (*ModelsService)(nil)

// NewModelsService builds a ModelsService reading settingsPath. Pass "" to
// use DefaultClaudeGatewaySettingsPath().
func NewModelsService(settingsPath string) *ModelsService {
	if settingsPath == "" {
		settingsPath = DefaultClaudeGatewaySettingsPath()
	}
	return &ModelsService{settingsPath: settingsPath}
}

// DefaultClaudeGatewaySettingsPath ports default_claude_gateway_settings_path
// (lib.rs:608-619): CLAUDE_GATEWAY_SETTINGS_PATH (full path) wins; else
// CLAUDE_GATEWAY_HOME/settings.json; else ~/.claude_gateway/settings.json.
// Per project CLAUDE.md, this is the packaged/Homebrew (cldg/clddg)
// location; ~/.claude_codex/settings.json (developer cldc/clddc mode) is
// not read here because the Rust source this ports from
// (default_claude_gateway_settings_path) never resolves to it either -
// only the CLAUDE_GATEWAY_* env vars or the packaged default.
func DefaultClaudeGatewaySettingsPath() string {
	if path := os.Getenv("CLAUDE_GATEWAY_SETTINGS_PATH"); path != "" {
		return path
	}
	if home := os.Getenv("CLAUDE_GATEWAY_HOME"); home != "" {
		return filepath.Join(home, "settings.json")
	}
	homeDir, err := os.UserHomeDir()
	if err != nil {
		homeDir = "."
	}
	return filepath.Join(homeDir, ".claude_gateway", "settings.json")
}

// List reads and parses the settings file, then builds the model catalog.
// A missing/unparseable file or an empty resulting catalog both surface as
// a CodeAPI AppError (mirroring the Rust handler's INTERNAL_SERVER_ERROR
// config_error response for both cases).
func (s *ModelsService) List(_ context.Context) (*dto.ModelList, error) {
	settings, err := loadClaudeGatewaySettings(s.settingsPath)
	if err != nil {
		return nil, apperr.Wrap(err, apperr.CodeAPI, "load claude gateway settings", 500)
	}

	data := modelCatalogFromSettings(settings)
	if len(data) == 0 {
		return nil, apperr.New(apperr.CodeAPI, fmt.Sprintf("no models were found in %s", s.settingsPath), 500)
	}

	return &dto.ModelList{Object: "list", Data: data}, nil
}

func loadClaudeGatewaySettings(path string) (*claudeGatewaySettings, error) {
	raw, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("failed to read Claude gateway settings at %s: %w", path, err)
	}
	var settings claudeGatewaySettings
	if err := json.Unmarshal(raw, &settings); err != nil {
		return nil, fmt.Errorf("failed to parse Claude gateway settings at %s: %w", path, err)
	}
	return &settings, nil
}

func modelCatalogFromSettings(settings *claudeGatewaySettings) []dto.Model {
	if len(settings.Models) > 0 {
		envWindows := modelContextWindowsFromEnv(settings.Env)
		out := make([]dto.Model, 0, len(settings.Models))
		for _, m := range settings.Models {
			maxTokens := m.MaxInputTokens
			if maxTokens == nil {
				if w, ok := envWindows[m.ID]; ok {
					maxTokens = &w
				}
			}
			out = append(out, dto.Model{
				ID:             m.ID,
				Type:           "model",
				Name:           m.Name,
				Description:    m.Description,
				MaxInputTokens: maxTokens,
			})
		}
		return dedupeModels(out)
	}

	var out []dto.Model
	for _, slot := range fallbackModelSlots {
		id, ok := envString(settings.Env, slot.keys.id)
		if !ok {
			continue
		}
		name := slot.defaultName
		if v, ok := envString(settings.Env, slot.keys.name); ok {
			name = v
		}
		descr := slot.defaultDescr
		if v, ok := envString(settings.Env, slot.keys.description); ok {
			descr = v
		}
		out = append(out, dto.Model{
			ID:             id,
			Type:           "model",
			Name:           &name,
			Description:    &descr,
			MaxInputTokens: envUint64(settings.Env, slot.keys.maxInputTokens),
		})
	}
	return dedupeModels(out)
}

func modelContextWindowsFromEnv(env map[string]any) map[string]uint64 {
	pairs := [][2]string{
		{"ANTHROPIC_DEFAULT_HAIKU_MODEL", "ANTHROPIC_DEFAULT_HAIKU_MAX_TOKENS"},
		{"ANTHROPIC_DEFAULT_SONNET_MODEL", "ANTHROPIC_DEFAULT_SONNET_MAX_TOKENS"},
		{"ANTHROPIC_DEFAULT_OPUS_MODEL", "ANTHROPIC_DEFAULT_OPUS_MAX_TOKENS"},
		{"ANTHROPIC_DEFAULT_FABLE_MODEL", "ANTHROPIC_DEFAULT_FABLE_MAX_TOKENS"},
		{"ANTHROPIC_CUSTOM_MODEL_OPTION", "ANTHROPIC_CUSTOM_MODEL_OPTION_MAX_TOKENS"},
	}
	out := map[string]uint64{}
	for _, p := range pairs {
		modelID, ok := envString(env, p[0])
		if !ok {
			continue
		}
		maxTokens := envUint64(env, p[1])
		if maxTokens == nil {
			continue
		}
		out[modelID] = *maxTokens
	}
	return out
}

func envString(env map[string]any, key string) (string, bool) {
	v, ok := env[key]
	if !ok {
		return "", false
	}
	s, ok := v.(string)
	return s, ok
}

func envUint64(env map[string]any, key string) *uint64 {
	v, ok := env[key]
	if !ok {
		return nil
	}
	switch n := v.(type) {
	case float64:
		u := uint64(n)
		return &u
	case string:
		var u uint64
		if _, err := fmt.Sscanf(n, "%d", &u); err == nil {
			return &u
		}
	}
	return nil
}

func dedupeModels(models []dto.Model) []dto.Model {
	seen := make(map[string]bool, len(models))
	out := make([]dto.Model, 0, len(models))
	for _, m := range models {
		if seen[m.ID] {
			continue
		}
		seen[m.ID] = true
		out = append(out, m)
	}
	return out
}
