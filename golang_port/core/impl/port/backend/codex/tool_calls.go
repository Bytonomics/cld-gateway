package codex

import (
	"encoding/json"
	"strings"
)

// ToolCallKind mirrors Rust CodexToolCallKind.
type ToolCallKind string

const (
	ToolCallKindFunction   ToolCallKind = "function"
	ToolCallKindCustom     ToolCallKind = "custom"
	ToolCallKindToolSearch ToolCallKind = "tool_search"
	ToolCallKindLocalShell ToolCallKind = "local_shell"
)

// ToolCall is the decoded shape of a backend tool-call invocation, mirroring
// Rust CodexToolCall (crates/gateway-backend-codex/src/types.rs). Callers map
// this into state.StoredToolCall by adding CallID/ToolName/ToolKind plus the
// request-scoped fields (RequestID, CreatedAtUnixSeconds).
type ToolCall struct {
	CallID    string
	Name      string
	Arguments string
	Kind      ToolCallKind
}

// ParseOutputItemToolCall extracts a tool call from a
// response.output_item.done/added event payload, if the item is a
// client-executed tool call (function_call, custom_tool_call,
// tool_search_call, local_shell_call). Hosted calls (e.g. web_search_call)
// return nil.
func ParseOutputItemToolCall(eventName, data string) *ToolCall {
	if eventName != "response.output_item.done" && eventName != "response.output_item.added" {
		return nil
	}

	var value map[string]any
	if err := json.Unmarshal([]byte(data), &value); err != nil {
		return nil
	}

	item, ok := value["item"].(map[string]any)
	if !ok {
		if response, ok := value["response"].(map[string]any); ok {
			item, ok = response["item"].(map[string]any)
			if !ok {
				return nil
			}
		} else {
			return nil
		}
	}

	return ParseToolCallItem(item)
}

// ParseToolCallItem decodes a single output item into a ToolCall.
func ParseToolCallItem(item map[string]any) *ToolCall {
	itemType, ok := item["type"].(string)
	if !ok {
		return nil
	}
	switch itemType {
	case "function_call":
		return parseFunctionCall(item)
	case "custom_tool_call":
		return parseCustomToolCall(item)
	case "tool_search_call":
		return parseToolSearchCall(item)
	case "local_shell_call":
		return parseLocalShellCall(item)
	default:
		return nil
	}
}

func parseFunctionCall(item map[string]any) *ToolCall {
	callID, ok := item["call_id"].(string)
	if !ok {
		return nil
	}
	name, ok := item["name"].(string)
	if !ok {
		return nil
	}
	rawArguments, _ := item["arguments"].(string)
	return &ToolCall{
		CallID:    callID,
		Name:      name,
		Arguments: NormalizeJSONObjectString(rawArguments, "arguments"),
		Kind:      ToolCallKindFunction,
	}
}

func parseCustomToolCall(item map[string]any) *ToolCall {
	callID, ok := item["call_id"].(string)
	if !ok {
		return nil
	}
	name, ok := item["name"].(string)
	if !ok {
		return nil
	}
	input, _ := item["input"].(string)
	arguments, err := json.Marshal(map[string]any{"input": input})
	if err != nil {
		arguments = []byte("{}")
	}
	return &ToolCall{
		CallID:    callID,
		Name:      name,
		Arguments: string(arguments),
		Kind:      ToolCallKindCustom,
	}
}

func parseToolSearchCall(item map[string]any) *ToolCall {
	callID, ok := item["call_id"].(string)
	if !ok {
		return nil
	}
	arguments, ok := item["arguments"]
	if !ok {
		arguments = map[string]any{}
	}
	return &ToolCall{
		CallID:    callID,
		Name:      "tool_search",
		Arguments: NormalizeJSONValueObjectString(arguments, "arguments"),
		Kind:      ToolCallKindToolSearch,
	}
}

func parseLocalShellCall(item map[string]any) *ToolCall {
	callID, ok := item["call_id"].(string)
	if !ok {
		return nil
	}
	args := map[string]any{}
	if status, ok := item["status"].(string); ok {
		args["status"] = status
	}
	if action, ok := item["action"]; ok {
		args["action"] = action
	}
	arguments, err := json.Marshal(args)
	if err != nil {
		arguments = []byte("{}")
	}
	return &ToolCall{
		CallID:    callID,
		Name:      "local_shell",
		Arguments: string(arguments),
		Kind:      ToolCallKindLocalShell,
	}
}

// NormalizeJSONObjectString parses raw as JSON; if it decodes to an object,
// that object (re-encoded) is returned. Otherwise raw is wrapped as
// {fallbackField: raw}. Empty/whitespace-only raw becomes "{}".
func NormalizeJSONObjectString(raw, fallbackField string) string {
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
	return NormalizeJSONValueObjectString(value, fallbackField)
}

// NormalizeJSONValueObjectString returns value re-encoded as a JSON object;
// if value is not itself an object, it is wrapped as {fallbackField: value}.
func NormalizeJSONValueObjectString(value any, fallbackField string) string {
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
