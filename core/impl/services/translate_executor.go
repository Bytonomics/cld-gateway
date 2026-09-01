// Package services holds core use-case orchestrators. This file ports
// crates/gateway-http-anthropic/src/translate_executor.rs:78-237: the
// local-command status executor that produces a Gateway-owned status
// document for translated Claude Code slash commands (currently "/status"),
// with non-blocking live usage enrichment and rate-limit/spend-control
// normalization.
package services

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"strings"
	"time"

	"github.com/Bytonomics/cld-gateway/core"
	"github.com/Bytonomics/cld-gateway/netpolicy"
)

// SessionInfo carries Gateway's local session/thread information for status
// display, mirroring translate_executor.rs SessionInfo (:28-33).
type SessionInfo struct {
	ThreadID       *string
	ThreadName     *string
	AccountDisplay *string
}

// ExecutorRuntime is the runtime context passed to executor functions,
// mirroring translate_executor.rs ExecutorRuntime (:8-25). It intentionally
// depends only on core.Secret/netpolicy.Client rather than the codex backend
// client package, keeping this file free of a codex-specific import.
type ExecutorRuntime struct {
	HasCredentials  bool
	AccessToken     core.Secret
	AccountID       string
	BaseURL         string
	HTTPClient      *netpolicy.Client
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

// normalizeCommandName strips leading/trailing whitespace and a leading
// slash, matching the Rust normalization at translate_executor.rs:62.
func normalizeCommandName(name string) string {
	return strings.TrimPrefix(strings.TrimSpace(name), "/")
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
// best-effort and never fails the executor.
func executeStatusCommand(ctx context.Context, runtime *ExecutorRuntime) map[string]any {
	baseURL := strings.TrimRight(runtime.BaseURL, "/")

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
			"base_url": baseURL,
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
	// failure (:126).
	if usageData, err := fetchLiveUsageData(ctx, runtime); err == nil {
		if planType, ok := usageData["plan_type"]; ok {
			status["plan_type"] = planType
		}
		status["rate_limits"] = normalizeRateLimits(usageData)
		if spend, ok := usageData["spend_control"].(map[string]any); ok {
			status["spend_control"] = normalizeSpendControl(spend)
		}
		status["usage_raw"] = usageData
		status["usage_state"] = "current"
	} else {
		status["usage_state"] = "stale_or_unavailable"
	}

	return status
}

// fetchLiveUsageData fetches live usage/rate-limit data from the upstream
// Codex API, mirroring fetch_live_usage_data (:150-183).
func fetchLiveUsageData(ctx context.Context, runtime *ExecutorRuntime) (map[string]any, error) {
	if !runtime.HasCredentials {
		return nil, errors.New("credentials_unavailable")
	}

	baseURL := strings.TrimRight(runtime.BaseURL, "/")
	url := baseURL + "/api/codex/usage"

	req, err := http.NewRequestWithContext(ctx, http.MethodGet, url, nil)
	if err != nil {
		return nil, fmt.Errorf("usage_fetch_policy_error: %w", err)
	}
	req.Header.Set("Authorization", "Bearer "+runtime.AccessToken.Expose())
	req.Header.Set("chatgpt-account-id", runtime.AccountID)

	httpClient := runtime.HTTPClient
	if httpClient == nil {
		httpClient = netpolicy.New(nil)
	}

	res, err := httpClient.Do(req)
	if err != nil {
		return nil, fmt.Errorf("usage_fetch_transport_error: %w", err)
	}
	defer func() { _ = res.Body.Close() }()

	if res.StatusCode < 200 || res.StatusCode >= 300 {
		return nil, fmt.Errorf("usage_fetch_status_%d", res.StatusCode)
	}

	body, err := io.ReadAll(res.Body)
	if err != nil {
		return nil, fmt.Errorf("usage_fetch_body_error: %w", err)
	}

	var parsed map[string]any
	if err := json.Unmarshal(body, &parsed); err != nil {
		return nil, fmt.Errorf("usage_fetch_parse_error: %w", err)
	}
	return parsed, nil
}

// normalizeRateLimits normalizes rate-limit data from the upstream usage
// response into stable Gateway-owned fields, mirroring normalize_rate_limits
// (:186-237).
func normalizeRateLimits(usage map[string]any) map[string]any {
	limits := map[string]any{}

	if rl, ok := usage["rate_limit"].(map[string]any); ok {
		primary := map[string]any{
			"allowed":       rl["allowed"],
			"limit_reached": rl["limit_reached"],
		}
		if pw, ok := rl["primary_window"].(map[string]any); ok {
			primary["used_percent"] = pw["used_percent"]
			primary["reset_at"] = pw["reset_at"]
			primary["window_seconds"] = pw["limit_window_seconds"]
		}
		limits["primary"] = primary

		if sw, ok := rl["secondary_window"].(map[string]any); ok {
			limits["secondary"] = map[string]any{
				"used_percent":   sw["used_percent"],
				"reset_at":       sw["reset_at"],
				"window_seconds": sw["limit_window_seconds"],
			}
		}
	}

	if additional, ok := usage["additional_rate_limits"].([]any); ok {
		entries := make([]any, 0, len(additional))
		for _, raw := range additional {
			entry, ok := raw.(map[string]any)
			if !ok {
				continue
			}
			name, ok := entry["limit_name"].(string)
			if !ok {
				continue
			}
			rl, ok := entry["rate_limit"].(map[string]any)
			if !ok {
				continue
			}
			pw, ok := rl["primary_window"].(map[string]any)
			if !ok {
				continue
			}
			entries = append(entries, map[string]any{
				"limit_name":     name,
				"allowed":        rl["allowed"],
				"limit_reached":  rl["limit_reached"],
				"used_percent":   pw["used_percent"],
				"reset_at":       pw["reset_at"],
				"window_seconds": pw["limit_window_seconds"],
			})
		}
		if len(entries) > 0 {
			limits["additional"] = entries
		}
	}

	return limits
}

// normalizeSpendControl normalizes spend-control data from the upstream
// usage response, mirroring normalize_spend_control (:240-256).
func normalizeSpendControl(spend map[string]any) map[string]any {
	result := map[string]any{
		"reached": spend["reached"],
	}
	if limit, ok := spend["individual_limit"].(map[string]any); ok {
		result["individual_limit"] = map[string]any{
			"source":            limit["source"],
			"limit":             limit["limit"],
			"used":              limit["used"],
			"remaining":         limit["remaining"],
			"used_percent":      limit["used_percent"],
			"remaining_percent": limit["remaining_percent"],
			"reset_at":          limit["reset_at"],
		}
	}
	return result
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
