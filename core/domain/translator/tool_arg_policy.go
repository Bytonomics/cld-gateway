package translator

import (
	"encoding/json"
	"fmt"
	"strings"
)

// ToolCallKind mirrors the backend tool-call kinds a translator needs to
// know about to parse a raw tool-call argument buffer (port of
// gateway-backend-codex::types::CodexToolCallKind). Kept as an independent
// domain-level type rather than importing the codex impl package, so this
// policy stays backend-agnostic per the translator port's extend-via-
// composition design (ARCHITECTURE_v2.md).
// Values match the canonical wire-format strings from
// crates/gateway-backend-codex/src/types.rs::CodexToolCallKind::as_str().
type ToolCallKind string

const (
	ToolCallKindFunction   ToolCallKind = "function_call"
	ToolCallKindCustom     ToolCallKind = "custom_tool_call"
	ToolCallKindToolSearch ToolCallKind = "tool_search_call"
	ToolCallKindLocalShell ToolCallKind = "local_shell_call"
)

// PolicyEdit records one tool-arg mutation applied by ApplyPolicies, for
// observability/audit. Port of tool_arg_policy.rs PolicyEdit.
type PolicyEdit struct {
	Field  string
	Action string
	Reason string
}

// ToolArgContext carries the inputs a policy needs to decide whether to
// act. Port of tool_arg_policy.rs ToolArgContext.
type ToolArgContext struct {
	ToolName string
}

// ApplyPolicies runs all tool-arg policies against args in place, returning
// the edits applied. Port of tool_arg_policy.rs apply_policies.
func ApplyPolicies(ctx ToolArgContext, args map[string]any) []PolicyEdit {
	edits := make([]PolicyEdit, 0)
	edits = append(edits, agentPolicy(ctx, args)...)
	edits = append(edits, readPolicy(ctx, args)...)
	return edits
}

// SanitizedToolArgsForKind parses buf into a JSON object appropriate for
// kind, then applies ApplyPolicies. Port of tool_arg_policy.rs
// sanitized_tool_args_for_kind.
func SanitizedToolArgsForKind(toolName string, kind ToolCallKind, buf string) (map[string]any, []PolicyEdit, error) {
	args, err := parseToolArgsObjectForKind(kind, buf)
	if err != nil {
		return nil, nil, err
	}
	ctx := ToolArgContext{ToolName: toolName}
	edits := ApplyPolicies(ctx, args)
	return args, edits, nil
}

func validateToolArgsJSONObject(buf string) error {
	trimmed := strings.TrimSpace(buf)
	if trimmed == "" {
		return nil
	}
	var value any
	if err := json.Unmarshal([]byte(trimmed), &value); err != nil {
		return fmt.Errorf("tool_use.input is not valid JSON: %w", err)
	}
	if _, ok := value.(map[string]any); !ok {
		return fmt.Errorf("tool_use.input must be a JSON object")
	}
	return nil
}

func parseToolArgsObject(buf string) (map[string]any, error) {
	trimmed := strings.TrimSpace(buf)
	if err := validateToolArgsJSONObject(trimmed); err != nil {
		return nil, err
	}
	if trimmed == "" {
		return map[string]any{}, nil
	}
	var value any
	if err := json.Unmarshal([]byte(trimmed), &value); err != nil {
		return nil, fmt.Errorf("tool_use.input is not valid JSON: %w", err)
	}
	obj, ok := value.(map[string]any)
	if !ok {
		return nil, fmt.Errorf("tool_use.input must be a JSON object")
	}
	return obj, nil
}

func parseToolArgsObjectForKind(kind ToolCallKind, buf string) (map[string]any, error) {
	if kind != ToolCallKindCustom {
		return parseToolArgsObject(buf)
	}

	trimmed := strings.TrimSpace(buf)
	if trimmed == "" {
		return map[string]any{}, nil
	}
	if obj, err := parseToolArgsObject(trimmed); err == nil {
		return obj, nil
	}

	normalized := normalizeJSONObjectString(trimmed, "input")
	return parseToolArgsObject(normalized)
}

// normalizeJSONObjectString parses raw as JSON; if it decodes to an object,
// that object (re-encoded) is returned. Otherwise raw is wrapped as
// {fallbackField: raw}. Empty/whitespace-only raw becomes "{}". Port of
// gateway-backend-codex::tool_calls::normalize_json_object_string, kept
// local to this package for the same backend-agnostic reason as
// ToolCallKind above.
func normalizeJSONObjectString(raw, fallbackField string) string {
	trimmed := strings.TrimSpace(raw)
	if trimmed == "" {
		return "{}"
	}
	var value any
	if err := json.Unmarshal([]byte(trimmed), &value); err != nil {
		encoded, marshalErr := json.Marshal(map[string]any{fallbackField: raw})
		if marshalErr != nil {
			return "{}"
		}
		return string(encoded)
	}
	objectValue := value
	if _, ok := value.(map[string]any); !ok {
		objectValue = map[string]any{fallbackField: value}
	}
	encoded, err := json.Marshal(objectValue)
	if err != nil {
		return "{}"
	}
	return string(encoded)
}

func agentPolicy(ctx ToolArgContext, args map[string]any) []PolicyEdit {
	if ctx.ToolName != "Agent" {
		return nil
	}
	if _, ok := args["isolation"]; !ok {
		return nil
	}
	delete(args, "isolation")
	return []PolicyEdit{{
		Field:  "isolation",
		Action: "remove",
		Reason: "gateway should not force worktree isolation for Agent calls",
	}}
}

func readPolicy(ctx ToolArgContext, args map[string]any) []PolicyEdit {
	if ctx.ToolName != "Read" {
		return nil
	}

	edits := make([]PolicyEdit, 0)
	pages, hasPages := args["pages"]
	if !hasPages {
		return edits
	}

	filePath, _ := args["file_path"].(string)
	isPDF := strings.HasSuffix(strings.ToLower(filePath), ".pdf")

	pagesStr, pagesIsString := pages.(string)
	pagesEmpty := pagesIsString && pagesStr == ""

	if pagesEmpty {
		delete(args, "pages")
		edits = append(edits, PolicyEdit{
			Field:  "pages",
			Action: "remove",
			Reason: "empty string is invalid; omit pages unless reading a PDF",
		})
		return edits
	}

	if !isPDF {
		delete(args, "pages")
		edits = append(edits, PolicyEdit{
			Field:  "pages",
			Action: "remove",
			Reason: "pages only applies to PDF reads",
		})
	}

	return edits
}
