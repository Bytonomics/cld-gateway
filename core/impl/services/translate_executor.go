// Package services holds core use-case orchestrators. This file ports
// crates/gateway-http-anthropic/src/translate_executor.rs:78-237: the
// local-command status executor that produces a Gateway-owned status
// document for translated Claude Code slash commands (currently "/status"),
// with non-blocking live usage enrichment and rate-limit/spend-control
// normalization.
//
// This executor is backend-agnostic: it fetches usage/rate-limit data via
// backend.Backend.FetchStatusData, which every backend implements against
// its own status API and returns already normalized into the generic
// plan_type/rate_limits/spend_control/usage_raw shape below. Adding status
// support for a new backend never requires touching this file.
package services

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"
	"time"

	backendport "github.com/Bytonomics/cld-gateway/core/domain/port/backend"
)

// SessionInfo carries Gateway's local session/thread information for status
// display, mirroring translate_executor.rs SessionInfo (:28-33).
type SessionInfo struct {
	ThreadID       *string
	ThreadName     *string
	AccountDisplay *string
}

// ExecutorRuntime is the runtime context passed to executor functions,
// mirroring translate_executor.rs ExecutorRuntime (:8-25). Backend is the
// active backend.Backend implementation, used only through
// backend.Backend.FetchStatusData - this file never imports a concrete
// backend package.
type ExecutorRuntime struct {
	Backend         backendport.Backend
	HasCredentials  bool
	AccountID       string
	BackendName     string
	CurrentModel    *string
	SessionInfo     SessionInfo
	GatewayVersion  string
	ConfigPath      *string
	ResolvedModel   *string
	CurrentDir      *string
	ReasoningEffort *string
}

// PostResultFn is the post-result wrapper function signature: takes executor
// JSON and packaged command body, returns final output text. Mirrors
// PostResultFn (:36).
type PostResultFn func(executorJSON map[string]any, packagedBody string) string

// CommandExecutorNames is the registry of translated command names that have
// executor functions, mirroring COMMAND_EXECUTOR_NAMES (:40).
var CommandExecutorNames = []string{"status"}

// CommandPostResults maps normalized command name to its post-result wrapper
// function, mirroring COMMAND_POST_RESULTS (:45-46).
var CommandPostResults = map[string]PostResultFn{
	"status": PostResultForTranslatedCommand,
}

// gatewayPluginNamespace mirrors claudecode.gatewayPluginNamespace
// (core/domain/claudecode/commands.go): Claude Code prefixes every command
// owned by the packaged "gateway" plugin with this namespace on the wire
// (e.g. "/gateway:status", confirmed from a live <command-name> envelope
// tag), so lookups here against the bare CommandExecutorNames/
// CommandPostResults keys ("status") must strip it the same way
// claudecode's own classification does, or a plugin-owned command's
// executor silently never runs even after correctly classifying as
// Translate.
const gatewayPluginNamespace = "gateway:"

// normalizeCommandName strips leading/trailing whitespace, a leading
// slash, and the gateway plugin namespace, matching the Rust normalization
// at translate_executor.rs:62 (extended for the plugin-namespace prefix,
// which Rust never had to handle).
func normalizeCommandName(name string) string {
	name = strings.TrimPrefix(strings.TrimSpace(name), "/")
	name = strings.TrimPrefix(name, gatewayPluginNamespace)
	return name
}

// ExecuteTranslatedCommand executes a translated command if one is present,
// mirroring execute_translated_command (:53-76). commandName == nil mirrors
// Option::None. Returns (nil, nil) when no translated command was detected;
// an error return is reserved for a registered command whose execution
// fails explicitly (none do today, matching the Rust source).
func ExecuteTranslatedCommand(ctx context.Context, commandName *string, runtime *ExecutorRuntime) (map[string]any, error) {
	if commandName == nil {
		return nil, nil
	}

	normalized := normalizeCommandName(*commandName)

	found := false
	for _, n := range CommandExecutorNames {
		if n == normalized {
			found = true
			break
		}
	}
	if !found {
		return nil, nil
	}

	switch normalized {
	case "status":
		return executeStatusCommand(ctx, runtime), nil
	default:
		return nil, nil
	}
}

// executeStatusCommand builds a Gateway-owned status document with local
// session info and optional usage data, mirroring execute_status_command
// (:81-146). It returns immediately with local state; usage enrichment is
// best-effort (via the active backend's FetchStatusData) and never fails
// the executor.
func executeStatusCommand(ctx context.Context, runtime *ExecutorRuntime) map[string]any {
	accountID := "unavailable"
	if runtime.HasCredentials {
		accountID = runtime.AccountID
	}

	timestamp := time.Now().Unix()

	status := map[string]any{
		"status_type":  "gateway_status",
		"generated_at": timestamp,
		"gateway": map[string]any{
			"version":     runtime.GatewayVersion,
			"config_path": derefOrNil(runtime.ConfigPath),
			"current_dir": derefOrNil(runtime.CurrentDir),
		},
		"session": map[string]any{
			"thread_id":       derefOrNil(runtime.SessionInfo.ThreadID),
			"thread_name":     derefOrNil(runtime.SessionInfo.ThreadName),
			"account_display": derefOrNil(runtime.SessionInfo.AccountDisplay),
		},
		"model": map[string]any{
			"requested":        derefOrNil(runtime.CurrentModel),
			"resolved":         resolvedModelOrCurrent(runtime),
			"reasoning_effort": derefOrNil(runtime.ReasoningEffort),
		},
		"provider": map[string]any{
			"name": runtime.BackendName,
		},
		"auth": map[string]any{
			"account_id": accountID,
		},
		"usage_state":   "pending",
		"plan_type":     nil,
		"rate_limits":   nil,
		"spend_control": nil,
		"usage_raw":     nil,
	}

	// Errors are captured in the status document, not escalated as executor
	// failure (:126). usageData is already normalized into these generic
	// keys by the active backend's FetchStatusData - no backend-specific
	// parsing happens here.
	if runtime.Backend != nil {
		if usageData, err := runtime.Backend.FetchStatusData(ctx); err == nil {
			if v, ok := usageData["plan_type"]; ok {
				status["plan_type"] = v
			}
			if v, ok := usageData["rate_limits"]; ok {
				status["rate_limits"] = v
			}
			if v, ok := usageData["spend_control"]; ok {
				status["spend_control"] = v
			}
			if v, ok := usageData["usage_raw"]; ok {
				status["usage_raw"] = v
			}
			status["usage_state"] = "current"
		} else {
			status["usage_state"] = "stale_or_unavailable"
		}
	} else {
		status["usage_state"] = "stale_or_unavailable"
	}

	return status
}

// PostResultForTranslatedCommand wraps executor JSON result with packaged
// command instructions if provided, mirroring
// post_result_for_translated_command (:258-282). An empty packaged body
// (after trimming) yields JSON-only output, so empty packaged prompts never
// inject synthetic scaffolding.
func PostResultForTranslatedCommand(executorJSON map[string]any, packagedBody string) string {
	packagedTrimmed := strings.TrimSpace(packagedBody)

	jsonBytes, err := json.MarshalIndent(executorJSON, "", "  ")
	jsonStr := string(jsonBytes)
	if err != nil {
		jsonStr = fmt.Sprintf("%v", executorJSON)
	}

	if packagedTrimmed == "" {
		return jsonStr
	}
	return packagedTrimmed + "\n\n" + jsonStr
}

// GetPostResultFunction returns the post-result wrapper function for a
// translated command, if registered, mirroring get_post_result_function
// (:285-291).
func GetPostResultFunction(commandName string) (PostResultFn, bool) {
	normalized := normalizeCommandName(commandName)
	fn, ok := CommandPostResults[normalized]
	return fn, ok
}

func derefOrNil(s *string) any {
	if s == nil {
		return nil
	}
	return *s
}

func resolvedModelOrCurrent(runtime *ExecutorRuntime) any {
	if runtime.ResolvedModel != nil {
		return *runtime.ResolvedModel
	}
	if runtime.CurrentModel != nil {
		return *runtime.CurrentModel
	}
	return nil
}
