package translator

// Port of crates/gateway-http-anthropic/src/sse_bridge.rs (backend event ->
// Anthropic SSE event mapping) plus the unary response construction from
// crates/gateway-http-anthropic/src/lib.rs build_unary_messages_response
// (lib.rs:1549-1636). Implements the response-event half of
// BackendTranslator on *GenericBackendTranslator; generic.go implements the
// request-shaping half.
//
// Design note (deviation from Rust, forced by the pinned Go interface
// shape): Rust's StreamState is constructed by the caller (lib.rs) with a
// request_id, structured_output_schema and context_management value handed
// in explicitly, then threaded through map_backend_event calls by
// reference. The Go BackendTranslator interface
// (TranslateRequest/TranslateResponseEvent/BuildUnaryResponse) has no room
// for that per-request context on individual method calls, so it is
// carried as fields on GenericBackendTranslator instead (see generic.go);
// a translator instance is expected to be constructed fresh per request.
// TranslateResponseEvent lazily creates its internal bridgeState from
// those fields on first call. BuildUnaryResponse always builds a fresh
// bridgeState of its own (mirrors the fact that unary responses in Rust
// never touch a StreamState at all) and runs it over the full event list,
// reusing exactly the same event-mapping code path as streaming so both
// forms of transport stay behaviorally identical.

import (
	"context"
	"encoding/json"
	"fmt"
	"sort"
	"strings"
	"time"

	"github.com/google/uuid"

	"github.com/Bytonomics/cld-gateway/core/domain/dto"
	"github.com/Bytonomics/cld-gateway/core/domain/port/backend"
	"github.com/Bytonomics/cld-gateway/core/domain/port/state"
)

// blockState mirrors sse_bridge.rs BlockState (:16-19).
type blockState struct {
	index  uint32
	closed bool
}

// webSearchCallInfo mirrors sse_bridge.rs WebSearchCall (:21-27).
type webSearchCallInfo struct {
	callID          string
	serverToolUseID string
	query           *string
	results         []any
}

// tokenUsage mirrors gateway-backend-codex::types::CodexTokenUsage
// (crates/gateway-backend-codex/src/types.rs:67-74).
type tokenUsage struct {
	InputTokens           int64
	CachedInputTokens     int64
	OutputTokens          int64
	ReasoningOutputTokens int64
	TotalTokens           int64
	WebSearchRequests     int64
}

// parsedToolCall mirrors gateway-backend-codex::types::CodexToolCall
// (crates/gateway-backend-codex/src/types.rs:76-91), parsed locally rather
// than imported from core/impl/port/backend/codex: core/domain must not
// depend on core/impl, and this parsing only needs the OpenAI-Responses-
// shaped wire JSON that GenericBackendTranslator already treats as its
// generic event format.
type parsedToolCall struct {
	CallID    string
	Name      string
	Arguments string
	Kind      ToolCallKind
}

// bridgeState mirrors sse_bridge.rs StreamState (:29-138). finalTextBuffer
// has no Rust equivalent field: it always accumulates the full assistant
// text (structured-output mode or not), so BuildUnaryResponse can read the
// final text without re-deriving it from SSE-shaped side effects.
type bridgeState struct {
	nextBlockIndex uint32
	blocks         []blockState

	activeTextIndex            *uint32
	sawOutputTextDelta         bool
	structuredOutputSchema     any
	structuredOutputTextBuffer strings.Builder
	finalTextBuffer            strings.Builder

	activeThinkingIndex *uint32

	toolBlocksByCallID  map[string]uint32
	toolNameByCallID    map[string]string
	toolKindByCallID    map[string]ToolCallKind
	toolArgsBufByCallID map[string]string
	lastToolCallID      *string

	completedUsage            *tokenUsage
	contextManagement         any
	completedWebSearchCallIDs map[string]struct{}
	emittedWebSearchCallIDs   map[string]struct{}

	completed bool
}

func newBridgeState(structuredOutputSchema, contextManagement any) *bridgeState {
	return &bridgeState{
		toolBlocksByCallID:        map[string]uint32{},
		toolNameByCallID:          map[string]string{},
		toolKindByCallID:          map[string]ToolCallKind{},
		toolArgsBufByCallID:       map[string]string{},
		structuredOutputSchema:    structuredOutputSchema,
		contextManagement:         contextManagement,
		completedWebSearchCallIDs: map[string]struct{}{},
		emittedWebSearchCallIDs:   map[string]struct{}{},
	}
}

func (st *bridgeState) addBlock() uint32 {
	idx := st.nextBlockIndex
	st.nextBlockIndex++
	st.blocks = append(st.blocks, blockState{index: idx})
	return idx
}

func (st *bridgeState) openThinkingBlockIfNeeded() (uint32, bool) {
	if st.activeThinkingIndex != nil {
		return *st.activeThinkingIndex, false
	}
	idx := st.addBlock()
	st.activeThinkingIndex = &idx
	return idx, true
}

func (st *bridgeState) openTextBlockIfNeeded() (uint32, bool) {
	if st.activeTextIndex != nil {
		return *st.activeTextIndex, false
	}
	idx := st.addBlock()
	st.activeTextIndex = &idx
	return idx, true
}

func (st *bridgeState) ensureToolBlock(callID string) (uint32, bool) {
	if idx, ok := st.toolBlocksByCallID[callID]; ok {
		return idx, false
	}
	idx := st.addBlock()
	st.toolBlocksByCallID[callID] = idx
	if _, ok := st.toolArgsBufByCallID[callID]; !ok {
		st.toolArgsBufByCallID[callID] = ""
	}
	return idx, true
}

// --- SSE event constructors (sse_bridge.rs :140-312) ---

func sseEvent(name string, payload any) dto.SSEEvent {
	data, err := json.Marshal(payload)
	if err != nil {
		data = []byte("{}")
	}
	return dto.SSEEvent{Event: name, Data: data}
}

func contentBlockStartText(index uint32) dto.SSEEvent {
	return sseEvent("content_block_start", map[string]any{
		"type":  "content_block_start",
		"index": index,
		"content_block": map[string]any{
			"type": "text", "text": "",
		},
	})
}

func contentBlockStartToolUse(index uint32, callID, name string) dto.SSEEvent {
	return sseEvent("content_block_start", map[string]any{
		"type":  "content_block_start",
		"index": index,
		"content_block": map[string]any{
			"type": "tool_use", "id": callID, "name": name, "input": map[string]any{},
		},
	})
}

func contentBlockStartServerToolUse(index uint32, serverToolUseID string, query *string) dto.SSEEvent {
	input := map[string]any{}
	if query != nil {
		input = map[string]any{"query": *query}
	}
	return sseEvent("content_block_start", map[string]any{
		"type":  "content_block_start",
		"index": index,
		"content_block": map[string]any{
			"type": "server_tool_use", "id": serverToolUseID, "name": "web_search", "input": input,
		},
	})
}

func contentBlockStartWebSearchToolResult(index uint32, serverToolUseID string, results []any) dto.SSEEvent {
	if results == nil {
		results = []any{}
	}
	return sseEvent("content_block_start", map[string]any{
		"type":  "content_block_start",
		"index": index,
		"content_block": map[string]any{
			"type": "web_search_tool_result", "tool_use_id": serverToolUseID, "content": results,
		},
	})
}

func contentBlockStartThinking(index uint32) dto.SSEEvent {
	return sseEvent("content_block_start", map[string]any{
		"type":  "content_block_start",
		"index": index,
		"content_block": map[string]any{
			"type": "thinking", "thinking": "", "signature": "",
		},
	})
}

func contentBlockDeltaText(index uint32, text string) dto.SSEEvent {
	return sseEvent("content_block_delta", map[string]any{
		"type": "content_block_delta", "index": index,
		"delta": map[string]any{"type": "text_delta", "text": text},
	})
}

func contentBlockDeltaInputJSON(index uint32, delta string) dto.SSEEvent {
	return sseEvent("content_block_delta", map[string]any{
		"type": "content_block_delta", "index": index,
		"delta": map[string]any{"type": "input_json_delta", "partial_json": delta},
	})
}

func contentBlockDeltaThinking(index uint32, delta string) dto.SSEEvent {
	return sseEvent("content_block_delta", map[string]any{
		"type": "content_block_delta", "index": index,
		"delta": map[string]any{"type": "thinking_delta", "thinking": delta},
	})
}

func contentBlockStop(index uint32) dto.SSEEvent {
	return sseEvent("content_block_stop", map[string]any{"type": "content_block_stop", "index": index})
}

func messageDelta(stopReason string, usage *tokenUsage, contextManagement any) dto.SSEEvent {
	payload := map[string]any{
		"type":  "message_delta",
		"delta": map[string]any{"stop_reason": stopReason, "stop_sequence": nil},
	}
	if usage != nil {
		payload["usage"] = anthropicUsageValue(*usage)
	}
	if contextManagement != nil {
		payload["context_management"] = contextManagement
	}
	return sseEvent("message_delta", payload)
}

func anthropicUsageValue(usage tokenUsage) map[string]any {
	uncachedInputTokens := usage.InputTokens - usage.CachedInputTokens
	if uncachedInputTokens < 0 {
		uncachedInputTokens = 0
	}
	value := map[string]any{
		"input_tokens":                uncachedInputTokens,
		"cache_creation_input_tokens": 0,
		"cache_read_input_tokens":     usage.CachedInputTokens,
		"output_tokens":               usage.OutputTokens,
	}
	if usage.WebSearchRequests > 0 {
		value["server_tool_use"] = map[string]any{
			"web_search_requests": usage.WebSearchRequests,
			"web_fetch_requests":  0,
		}
	}
	return value
}

func messageStopEvent() dto.SSEEvent {
	return sseEvent("message_stop", map[string]any{"type": "message_stop"})
}

func errorEvent(message string) []dto.SSEEvent {
	return []dto.SSEEvent{sseEvent("error", map[string]any{
		"type":  "error",
		"error": map[string]any{"type": "backend_error", "message": message},
	})}
}

func toolArgsDeltaFromObject(index uint32, obj map[string]any) dto.SSEEvent {
	encoded, err := json.Marshal(obj)
	if err != nil {
		encoded = []byte("{}")
	}
	return contentBlockDeltaInputJSON(index, string(encoded))
}

// --- finalize (sse_bridge.rs :314-354) ---

func finalizeMessage(st *bridgeState) []dto.SSEEvent {
	st.completed = true
	var out []dto.SSEEvent

	if st.structuredOutputSchema != nil && strings.TrimSpace(st.structuredOutputTextBuffer.String()) != "" {
		text := CleanupStructuredOutputTextWithSchema(st.structuredOutputSchema, st.structuredOutputTextBuffer.String())
		if strings.TrimSpace(text) != "" {
			idx, started := st.openTextBlockIfNeeded()
			st.sawOutputTextDelta = true
			if started {
				out = append(out, contentBlockStartText(idx))
			}
			out = append(out, contentBlockDeltaText(idx, text))
		}
	}

	for i := range st.blocks {
		if st.blocks[i].closed {
			continue
		}
		out = append(out, contentBlockStop(st.blocks[i].index))
		st.blocks[i].closed = true
	}

	stopReason := "end_turn"
	if len(st.toolBlocksByCallID) > 0 {
		stopReason = "tool_use"
	}
	out = append(out, messageDelta(stopReason, st.completedUsage, st.contextManagement))
	out = append(out, messageStopEvent())
	return out
}

// --- delta-field parsing (sse_bridge.rs :356-368) ---

func parseDeltaEventFields(data string) (callID *string, delta string, ok bool) {
	var v map[string]any
	if err := json.Unmarshal([]byte(data), &v); err != nil {
		return nil, "", false
	}
	d, ok2 := v["delta"].(string)
	if !ok2 {
		return nil, "", false
	}
	if c, ok3 := v["call_id"].(string); ok3 {
		callID = &c
	}
	return callID, d, true
}

// --- main dispatcher (sse_bridge.rs :370-403) ---

func mapBackendEvent(st *bridgeState, eventName, data string, toolCalls state.ToolCallRepo, requestID *string, now func() time.Time) []dto.SSEEvent {
	if message, ok := parseBackendFailureEvent(eventName, data); ok {
		st.completed = true
		return errorEvent(fmt.Sprintf("backend stream failed: %s", message))
	}
	recordCompletedWebSearchCallIDs(st.completedWebSearchCallIDs, eventName, data)

	switch eventName {
	case "response.output_text.delta":
		return handleOutputTextDelta(st, data)
	case "response.output_item.added", "response.output_item.done":
		return handleOutputItem(st, eventName, data, toolCalls, requestID, now)
	case "response.web_search_call.completed":
		return handleWebSearchCallEvent(st, eventName, data)
	case "response.function_call_arguments.delta", "response.custom_tool_call_input.delta":
		return handleToolArgDelta(st, data)
	case "response.reasoning_text.delta", "response.reasoning_summary_text.delta":
		return handleReasoningDelta(st, data)
	case "response.completed":
		return handleCompleted(st, data)
	default:
		return nil
	}
}

// --- backend failure detection, port of gateway-backend-codex::backend_error::parse_backend_failure_event ---

func parseBackendFailureEvent(eventName, data string) (string, bool) {
	if eventName != "error" && eventName != "response.failed" {
		return "", false
	}
	var value any
	if err := json.Unmarshal([]byte(data), &value); err != nil {
		trimmed := strings.TrimSpace(data)
		if trimmed == "" {
			return "", false
		}
		return fmt.Sprintf("%s: %s", eventName, trimmed), true
	}
	message, ok := findStringField(value, "message")
	if !ok {
		message, ok = findStringField(value, "error")
	}
	if !ok {
		message, ok = findStringField(value, "code")
	}
	if !ok {
		encoded, err := json.Marshal(value)
		if err != nil {
			message = "unparseable backend error"
		} else {
			message = string(encoded)
		}
	}
	return fmt.Sprintf("%s: %s", eventName, message), true
}

func findStringField(value any, field string) (string, bool) {
	switch v := value.(type) {
	case map[string]any:
		if s, ok := v[field].(string); ok {
			return s, true
		}
		keys := make([]string, 0, len(v))
		for k := range v {
			keys = append(keys, k)
		}
		sort.Strings(keys)
		for _, k := range keys {
			if s, ok := findStringField(v[k], field); ok {
				return s, true
			}
		}
		return "", false
	case []any:
		for _, c := range v {
			if s, ok := findStringField(c, field); ok {
				return s, true
			}
		}
		return "", false
	default:
		return "", false
	}
}

// --- response.output_text.delta (sse_bridge.rs :405-427) ---

func handleOutputTextDelta(st *bridgeState, data string) []dto.SSEEvent {
	text, ok := extractDeltaText(data)
	if !ok || strings.TrimSpace(text) == "" {
		return nil
	}
	if st.structuredOutputSchema != nil {
		st.structuredOutputTextBuffer.WriteString(text)
		st.finalTextBuffer.WriteString(text)
		st.sawOutputTextDelta = true
		return nil
	}
	idx, started := st.openTextBlockIfNeeded()
	st.sawOutputTextDelta = true
	st.finalTextBuffer.WriteString(text)
	var out []dto.SSEEvent
	if started {
		out = append(out, contentBlockStartText(idx))
	}
	out = append(out, contentBlockDeltaText(idx, text))
	return out
}

// extractDeltaText ports gateway-backend-codex::output_text::extract_text_from_data
// (crates/gateway-backend-codex/src/output_text.rs:4-93).
func extractDeltaText(data string) (string, bool) {
	var value any
	if err := json.Unmarshal([]byte(data), &value); err != nil {
		return data, true
	}
	var last string
	var found bool
	extractLastTextFromValue(value, &last, &found)
	return last, found
}

func extractLastTextFromValue(value any, last *string, found *bool) {
	switch v := value.(type) {
	case map[string]any:
		if s, ok := v["text"].(string); ok {
			*last = s
			*found = true
		}
		if s, ok := v["delta"].(string); ok {
			*last = s
			*found = true
		}
		if content, ok := v["content"]; ok {
			extractLastTextFromValue(content, last, found)
		}
		keys := make([]string, 0, len(v))
		for k := range v {
			if k == "text" || k == "delta" || k == "content" {
				continue
			}
			keys = append(keys, k)
		}
		sort.Strings(keys)
		for _, k := range keys {
			extractLastTextFromValue(v[k], last, found)
		}
	case []any:
		for _, child := range v {
			extractLastTextFromValue(child, last, found)
		}
	default:
	}
}

// --- response.output_item.added / .done (sse_bridge.rs :429-516) ---

func handleOutputItem(st *bridgeState, eventName, data string, toolCalls state.ToolCallRepo, requestID *string, now func() time.Time) []dto.SSEEvent {
	if tc := parseOutputItemToolCall(eventName, data); tc != nil {
		return handleToolCallItem(st, *tc, eventName == "response.output_item.done", toolCalls, requestID, now)
	}

	var v map[string]any
	if err := json.Unmarshal([]byte(data), &v); err != nil {
		return nil
	}
	item, ok := v["item"].(map[string]any)
	if !ok {
		return nil
	}
	switch t, _ := item["type"].(string); t {
	case "web_search_call":
		return handleWebSearchCallItem(st, item)
	case "message":
		return handleMessageItem(st, item)
	default:
		return nil
	}
}

func handleToolCallItem(st *bridgeState, tc parsedToolCall, finalItem bool, toolCalls state.ToolCallRepo, requestID *string, now func() time.Time) []dto.SSEEvent {
	callID := tc.CallID
	toolIndex, isNew := st.ensureToolBlock(callID)
	idCopy := callID
	st.lastToolCallID = &idCopy
	st.toolNameByCallID[callID] = tc.Name
	st.toolKindByCallID[callID] = tc.Kind
	if finalItem {
		st.toolArgsBufByCallID[callID] = tc.Arguments
	}

	if !isNew {
		return nil
	}
	if toolCalls != nil {
		_ = toolCalls.RecordToolCall(context.Background(), state.StoredToolCall{
			CallID:               callID,
			ToolName:             tc.Name,
			ToolKind:             string(tc.Kind),
			RequestID:            requestID,
			CreatedAtUnixSeconds: now().Unix(),
		})
	}
	return []dto.SSEEvent{contentBlockStartToolUse(toolIndex, callID, tc.Name)}
}

// parseOutputItemToolCall ports
// gateway-backend-codex::tool_calls::parse_output_item_tool_call, kept
// local to this package for the same core/domain-vs-core/impl layering
// reason as parsedToolCall above.
func parseOutputItemToolCall(eventName, data string) *parsedToolCall {
	if eventName != "response.output_item.done" && eventName != "response.output_item.added" {
		return nil
	}
	var value map[string]any
	if err := json.Unmarshal([]byte(data), &value); err != nil {
		return nil
	}
	item, ok := value["item"].(map[string]any)
	if !ok {
		response, ok2 := value["response"].(map[string]any)
		if !ok2 {
			return nil
		}
		item, ok = response["item"].(map[string]any)
		if !ok {
			return nil
		}
	}
	return parseToolCallItem(item)
}

func parseToolCallItem(item map[string]any) *parsedToolCall {
	itemType, ok := item["type"].(string)
	if !ok {
		return nil
	}
	switch itemType {
	case "function_call":
		return parseFunctionCallItem(item)
	case "custom_tool_call":
		return parseCustomToolCallItem(item)
	case "tool_search_call":
		return parseToolSearchCallItem(item)
	case "local_shell_call":
		return parseLocalShellCallItem(item)
	default:
		return nil
	}
}

func parseFunctionCallItem(item map[string]any) *parsedToolCall {
	callID, ok := item["call_id"].(string)
	if !ok {
		return nil
	}
	name, ok := item["name"].(string)
	if !ok {
		return nil
	}
	rawArguments, _ := item["arguments"].(string)
	return &parsedToolCall{
		CallID:    callID,
		Name:      name,
		Arguments: normalizeJSONObjectString(rawArguments, "arguments"),
		Kind:      ToolCallKindFunction,
	}
}

func parseCustomToolCallItem(item map[string]any) *parsedToolCall {
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
	return &parsedToolCall{CallID: callID, Name: name, Arguments: string(arguments), Kind: ToolCallKindCustom}
}

func parseToolSearchCallItem(item map[string]any) *parsedToolCall {
	callID, ok := item["call_id"].(string)
	if !ok {
		return nil
	}
	arguments, ok := item["arguments"]
	if !ok {
		arguments = map[string]any{}
	}
	return &parsedToolCall{
		CallID:    callID,
		Name:      "tool_search",
		Arguments: normalizeJSONValueObjectString(arguments, "arguments"),
		Kind:      ToolCallKindToolSearch,
	}
}

func parseLocalShellCallItem(item map[string]any) *parsedToolCall {
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
	return &parsedToolCall{CallID: callID, Name: "local_shell", Arguments: string(arguments), Kind: ToolCallKindLocalShell}
}

// normalizeJSONValueObjectString mirrors normalizeJSONObjectString
// (tool_arg_policy.go) but starts from an already-decoded value rather
// than a raw string; port of
// gateway-backend-codex::tool_calls::normalize_json_value_object_string.
func normalizeJSONValueObjectString(value any, fallbackField string) string {
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

// --- message items (sse_bridge.rs :486-516) ---

func handleMessageItem(st *bridgeState, item map[string]any) []dto.SSEEvent {
	if st.sawOutputTextDelta {
		return nil
	}
	texts := messageItemOutputTexts(item)
	filtered := make([]string, 0, len(texts))
	for _, t := range texts {
		if strings.TrimSpace(t) != "" {
			filtered = append(filtered, t)
		}
	}
	if len(filtered) == 0 {
		return nil
	}
	if st.structuredOutputSchema != nil {
		for _, t := range filtered {
			st.structuredOutputTextBuffer.WriteString(t)
			st.finalTextBuffer.WriteString(t)
		}
		st.sawOutputTextDelta = true
		return nil
	}
	idx, started := st.openTextBlockIfNeeded()
	var out []dto.SSEEvent
	if started {
		out = append(out, contentBlockStartText(idx))
	}
	for _, t := range filtered {
		st.finalTextBuffer.WriteString(t)
		out = append(out, contentBlockDeltaText(idx, t))
	}
	return out
}

// messageItemOutputTexts ports gateway-backend-codex::output_text::message_item_output_texts
// (crates/gateway-backend-codex/src/output_text.rs:35-62).
func messageItemOutputTexts(item map[string]any) []string {
	itemType, _ := item["type"].(string)
	switch itemType {
	case "message":
		return outputTextsFromContentArray(item)
	case "output_text":
		if t, ok := item["text"].(string); ok && t != "" {
			return []string{t}
		}
		return nil
	default:
		return nil
	}
}

func outputTextsFromContentArray(item map[string]any) []string {
	content, ok := item["content"].([]any)
	if !ok {
		return nil
	}
	out := make([]string, 0, len(content))
	for _, c := range content {
		cm, ok := c.(map[string]any)
		if !ok {
			continue
		}
		if t, _ := cm["type"].(string); t != "output_text" {
			continue
		}
		if s, ok := cm["text"].(string); ok && s != "" {
			out = append(out, s)
		}
	}
	return out
}

// --- web search calls (sse_bridge.rs :518-668) ---

func handleWebSearchCallEvent(st *bridgeState, eventName, data string) []dto.SSEEvent {
	var event map[string]any
	if err := json.Unmarshal([]byte(data), &event); err != nil {
		return nil
	}
	call, ok := webSearchCallFromValue(eventName, event)
	if !ok {
		return nil
	}
	return emitWebSearchCallBlocks(st, *call)
}

func handleWebSearchCallItem(st *bridgeState, item map[string]any) []dto.SSEEvent {
	call, ok := webSearchCallFromItem(item, "output_item")
	if !ok {
		return nil
	}
	return emitWebSearchCallBlocks(st, *call)
}

func emitWebSearchCallBlocks(st *bridgeState, call webSearchCallInfo) []dto.SSEEvent {
	if _, seen := st.emittedWebSearchCallIDs[call.callID]; seen {
		return nil
	}
	st.emittedWebSearchCallIDs[call.callID] = struct{}{}

	serverToolIndex := st.addBlock()
	resultIndex := st.addBlock()
	return []dto.SSEEvent{
		contentBlockStartServerToolUse(serverToolIndex, call.serverToolUseID, call.query),
		contentBlockStop(serverToolIndex),
		contentBlockStartWebSearchToolResult(resultIndex, call.serverToolUseID, call.results),
		contentBlockStop(resultIndex),
	}
}

func webSearchCallFromValue(eventName string, event map[string]any) (*webSearchCallInfo, bool) {
	if t, _ := event["type"].(string); t != eventName {
		return nil, false
	}
	callID := firstString(event, "id", "call_id", "item_id")
	if callID == "" {
		if idx, ok := jsonNumberInt64(event["output_index"]); ok {
			callID = fmt.Sprintf("%s:%d", eventName, idx)
		}
	}
	if callID == "" {
		callID = eventName
	}
	return &webSearchCallInfo{
		callID:          callID,
		serverToolUseID: serverToolUseID(callID),
		query:           webSearchQuery(event),
		results:         webSearchResults(event),
	}, true
}

func webSearchCallFromItem(item map[string]any, fallback string) (*webSearchCallInfo, bool) {
	if t, _ := item["type"].(string); t != "web_search_call" {
		return nil, false
	}
	if status, ok := item["status"].(string); ok && status != "completed" {
		return nil, false
	}
	callID := firstString(item, "id", "call_id", "item_id")
	if callID == "" {
		callID = fallback
	}
	return &webSearchCallInfo{
		callID:          callID,
		serverToolUseID: serverToolUseID(callID),
		query:           webSearchQuery(item),
		results:         webSearchResults(item),
	}, true
}

func serverToolUseID(callID string) string {
	var b strings.Builder
	for _, r := range callID {
		if (r >= 'a' && r <= 'z') || (r >= 'A' && r <= 'Z') || (r >= '0' && r <= '9') || r == '_' {
			b.WriteRune(r)
		}
	}
	safeSuffix := b.String()
	switch {
	case safeSuffix == "":
		return "srvtoolu_web_search"
	case strings.HasPrefix(safeSuffix, "srvtoolu_"):
		return safeSuffix
	default:
		return "srvtoolu_" + safeSuffix
	}
}

func webSearchQuery(value map[string]any) *string {
	if q, ok := mapGetPath(value, "action", "query").(string); ok {
		return &q
	}
	if q, ok := mapGetPath(value, "input", "query").(string); ok {
		return &q
	}
	if q, ok := value["query"].(string); ok {
		return &q
	}
	return nil
}

func webSearchResults(value map[string]any) []any {
	sources := mapGetPath(value, "action", "sources")
	if sources == nil {
		sources = value["sources"]
	}
	arr, ok := sources.([]any)
	if !ok {
		return []any{}
	}
	out := make([]any, 0, len(arr))
	for _, s := range arr {
		if r, ok := webSearchResultFromSource(s); ok {
			out = append(out, r)
		}
	}
	return out
}

func webSearchResultFromSource(source any) (map[string]any, bool) {
	m, ok := source.(map[string]any)
	if !ok {
		return nil, false
	}
	url, ok := m["url"].(string)
	if !ok {
		return nil, false
	}
	result := map[string]any{"type": "web_search_result", "url": url}
	if title, ok := m["title"].(string); ok {
		result["title"] = title
	}
	if pageAge, ok := m["page_age"].(string); ok {
		result["page_age"] = pageAge
	}
	if enc, ok := m["encrypted_content"].(string); ok {
		result["encrypted_content"] = enc
	}
	return result, true
}

func mapGetPath(v any, keys ...string) any {
	cur := v
	for _, k := range keys {
		m, ok := cur.(map[string]any)
		if !ok {
			return nil
		}
		cur = m[k]
	}
	return cur
}

func firstString(m map[string]any, keys ...string) string {
	for _, k := range keys {
		if s, ok := m[k].(string); ok {
			return s
		}
	}
	return ""
}

func jsonNumberInt64(v any) (int64, bool) {
	f, ok := v.(float64)
	if !ok {
		return 0, false
	}
	return int64(f), true
}

// completed_web_search_call_ids family, port of gateway-backend-codex::sse_unary
// (crates/gateway-backend-codex/src/sse_unary.rs:253-329).

func recordCompletedWebSearchCallIDs(set map[string]struct{}, eventName, data string) {
	for _, id := range completedWebSearchCallIDsFor(eventName, data) {
		set[id] = struct{}{}
	}
}

func completedWebSearchCallIDsFor(eventName, data string) []string {
	var value map[string]any
	if err := json.Unmarshal([]byte(data), &value); err != nil {
		return nil
	}
	switch eventName {
	case "response.output_item.done":
		item, ok := value["item"].(map[string]any)
		if !ok {
			return nil
		}
		if id, ok := completedWebSearchCallID(item, "output_item"); ok {
			return []string{id}
		}
		return nil
	case "response.web_search_call.completed":
		if id, ok := eventWebSearchCallID(value, eventName); ok {
			return []string{id}
		}
		return nil
	case "response.completed":
		response, ok := value["response"].(map[string]any)
		if !ok {
			return nil
		}
		items, _ := response["output"].([]any)
		out := make([]string, 0, len(items))
		for i, it := range items {
			itemMap, ok := it.(map[string]any)
			if !ok {
				continue
			}
			if id, ok := completedWebSearchCallID(itemMap, fmt.Sprintf("response_output_%d", i)); ok {
				out = append(out, id)
			}
		}
		return out
	default:
		return nil
	}
}

func completedWebSearchCallID(item map[string]any, fallback string) (string, bool) {
	if t, _ := item["type"].(string); t != "web_search_call" {
		return "", false
	}
	if status, ok := item["status"].(string); ok && status != "completed" {
		return "", false
	}
	if id := firstString(item, "id", "call_id", "item_id"); id != "" {
		return id, true
	}
	return fallback, true
}

func eventWebSearchCallID(event map[string]any, fallback string) (string, bool) {
	if t, _ := event["type"].(string); t != fallback {
		return "", false
	}
	if id := firstString(event, "id", "call_id", "item_id"); id != "" {
		return id, true
	}
	if idx, ok := jsonNumberInt64(event["output_index"]); ok {
		return fmt.Sprintf("%s:%d", fallback, idx), true
	}
	return fallback, true
}

func webSearchCallsFromCompleted(data string) []webSearchCallInfo {
	var value map[string]any
	if err := json.Unmarshal([]byte(data), &value); err != nil {
		return nil
	}
	response, ok := value["response"].(map[string]any)
	if !ok {
		return nil
	}
	items, _ := response["output"].([]any)
	out := make([]webSearchCallInfo, 0, len(items))
	for i, it := range items {
		itemMap, ok := it.(map[string]any)
		if !ok {
			continue
		}
		if call, ok := webSearchCallFromItem(itemMap, fmt.Sprintf("response_output_%d", i)); ok {
			out = append(out, *call)
		}
	}
	return out
}

// --- tool-arg deltas (sse_bridge.rs :670-686) ---

func handleToolArgDelta(st *bridgeState, data string) []dto.SSEEvent {
	callIDPtr, delta, ok := parseDeltaEventFields(data)
	if !ok {
		return nil
	}
	var callID string
	switch {
	case callIDPtr != nil:
		callID = *callIDPtr
	case st.lastToolCallID != nil:
		callID = *st.lastToolCallID
	default:
		return nil
	}

	idx, isNew := st.ensureToolBlock(callID)
	st.toolArgsBufByCallID[callID] += delta

	if isNew {
		return []dto.SSEEvent{contentBlockStartToolUse(idx, callID, "")}
	}
	return nil
}

// --- reasoning deltas (sse_bridge.rs :688-697) ---

func handleReasoningDelta(st *bridgeState, data string) []dto.SSEEvent {
	_, delta, ok := parseDeltaEventFields(data)
	if !ok {
		return nil
	}
	idx, started := st.openThinkingBlockIfNeeded()
	var out []dto.SSEEvent
	if started {
		out = append(out, contentBlockStartThinking(idx))
	}
	out = append(out, contentBlockDeltaThinking(idx, delta))
	return out
}

// --- response.completed (sse_bridge.rs :699-768) ---

func handleCompleted(st *bridgeState, data string) []dto.SSEEvent {
	st.completedUsage = extractUsageFromCompletedEvent(data)
	if st.completedUsage != nil {
		st.completedUsage.WebSearchRequests = int64(len(st.completedWebSearchCallIDs))
	}

	var out []dto.SSEEvent
	for _, call := range webSearchCallsFromCompleted(data) {
		out = append(out, emitWebSearchCallBlocks(st, call)...)
	}

	type toolIndexPair struct {
		callID string
		index  uint32
	}
	pairs := make([]toolIndexPair, 0, len(st.toolBlocksByCallID))
	for callID, idx := range st.toolBlocksByCallID {
		pairs = append(pairs, toolIndexPair{callID, idx})
	}
	sort.Slice(pairs, func(i, j int) bool { return pairs[i].index < pairs[j].index })

	for _, p := range pairs {
		buf := st.toolArgsBufByCallID[p.callID]
		kind, ok := st.toolKindByCallID[p.callID]
		if !ok {
			kind = ToolCallKindFunction
		}
		toolName := st.toolNameByCallID[p.callID]
		obj, _, err := SanitizedToolArgsForKind(toolName, kind, buf)
		if err != nil {
			return errorEvent(err.Error())
		}
		out = append(out, toolArgsDeltaFromObject(p.index, obj))
	}

	out = append(out, finalizeMessage(st)...)
	return out
}

// extractUsageFromCompletedEvent ports
// gateway-backend-codex::sse_unary::extract_usage_from_completed_event
// (crates/gateway-backend-codex/src/sse_unary.rs:224-244).
func extractUsageFromCompletedEvent(data string) *tokenUsage {
	var value map[string]any
	if err := json.Unmarshal([]byte(data), &value); err != nil {
		return nil
	}
	response, ok := value["response"].(map[string]any)
	if !ok {
		return nil
	}
	usageRaw, ok := response["usage"].(map[string]any)
	if !ok {
		return nil
	}
	inputTokens, ok := jsonNumberInt64(usageRaw["input_tokens"])
	if !ok {
		return nil
	}
	outputTokens, ok := jsonNumberInt64(usageRaw["output_tokens"])
	if !ok {
		return nil
	}
	var cached int64
	if details, ok := usageRaw["input_tokens_details"].(map[string]any); ok {
		if c, ok := jsonNumberInt64(details["cached_tokens"]); ok {
			cached = c
		}
	}
	var reasoning int64
	if details, ok := usageRaw["output_tokens_details"].(map[string]any); ok {
		if r, ok := jsonNumberInt64(details["reasoning_tokens"]); ok {
			reasoning = r
		}
	}
	total := inputTokens + outputTokens
	if t, ok := jsonNumberInt64(usageRaw["total_tokens"]); ok {
		total = t
	}
	return &tokenUsage{
		InputTokens:           inputTokens,
		CachedInputTokens:     cached,
		OutputTokens:          outputTokens,
		ReasoningOutputTokens: reasoning,
		TotalTokens:           total,
		WebSearchRequests:     0,
	}
}

// --- GenericBackendTranslator methods (BackendTranslator response half) ---

func (g *GenericBackendTranslator) now() time.Time {
	if g.Clock != nil {
		return g.Clock.Now()
	}
	return time.Now()
}

// TranslateResponseEvent ports map_backend_event (sse_bridge.rs:370-403),
// driven one backend event at a time against a bridgeState that lives on
// the receiver for the lifetime of one request's stream (see the package
// doc comment above for why the state moved from a caller-owned value to a
// receiver field).
func (g *GenericBackendTranslator) TranslateResponseEvent(ev backend.Event) ([]dto.SSEEvent, error) {
	if g.stream == nil {
		g.stream = newBridgeState(g.StructuredOutputSchema, g.ContextManagementValue)
	}
	if g.stream.completed {
		return nil, nil
	}
	return mapBackendEvent(g.stream, ev.Type, string(ev.Data), g.ToolCalls, g.RequestID, g.now), nil
}

// BuildUnaryResponse ports build_unary_messages_response (lib.rs:1549-1636)
// and tool_call_content_block (lib.rs:1620-1636). It replays events through
// the same mapBackendEvent state machine TranslateResponseEvent uses (on a
// bridgeState of its own, discarding the SSE events that machine would
// have produced) purely to get identical text/tool-call/usage bookkeeping,
// then assembles the unary response shape directly, matching Rust's
// unconditional (schema-or-not) structured-output cleanup pass over the
// final accumulated text.
func (g *GenericBackendTranslator) BuildUnaryResponse(events []backend.Event) (*dto.MessagesResponse, error) {
	st := newBridgeState(g.StructuredOutputSchema, nil)
	for _, ev := range events {
		if st.completed {
			break
		}
		mapBackendEvent(st, ev.Type, string(ev.Data), g.ToolCalls, g.RequestID, g.now)
	}

	assistantText := CleanupStructuredOutputTextWithSchema(g.StructuredOutputSchema, st.finalTextBuffer.String())

	usage := dto.Usage{}
	if st.completedUsage != nil {
		usage.InputTokens = int(st.completedUsage.InputTokens)
		usage.OutputTokens = int(st.completedUsage.OutputTokens)
		if st.completedUsage.WebSearchRequests > 0 {
			usage.ServerToolUse = &dto.ServerToolUse{
				WebSearchRequests: int(st.completedUsage.WebSearchRequests),
				WebFetchRequests:  0,
			}
		}
	}

	id := "msg_" + uuid.NewString()

	if len(st.toolBlocksByCallID) == 0 {
		endTurn := "end_turn"
		return &dto.MessagesResponse{
			ID:         id,
			Type:       "message",
			Role:       "assistant",
			Model:      g.ResponseModel,
			Content:    []dto.ContentBlock{{BlockType: "text", Text: &assistantText}},
			StopReason: &endTurn,
			Usage:      usage,
		}, nil
	}

	type toolIndexPair struct {
		callID string
		index  uint32
	}
	pairs := make([]toolIndexPair, 0, len(st.toolBlocksByCallID))
	for callID, idx := range st.toolBlocksByCallID {
		pairs = append(pairs, toolIndexPair{callID, idx})
	}
	sort.Slice(pairs, func(i, j int) bool { return pairs[i].index < pairs[j].index })

	content := make([]dto.ContentBlock, 0, len(pairs)+1)
	if strings.TrimSpace(assistantText) != "" {
		content = append(content, dto.ContentBlock{BlockType: "text", Text: &assistantText})
	}
	for _, p := range pairs {
		name := st.toolNameByCallID[p.callID]
		kind := st.toolKindByCallID[p.callID]
		buf := st.toolArgsBufByCallID[p.callID]
		input, _, err := SanitizedToolArgsForKind(name, kind, buf)
		if err != nil {
			input = map[string]any{}
		}
		callID := p.callID
		toolName := name
		content = append(content, dto.ContentBlock{BlockType: "tool_use", ID: &callID, Name: &toolName, Input: input})
	}

	toolUse := "tool_use"
	return &dto.MessagesResponse{
		ID:         id,
		Type:       "message",
		Role:       "assistant",
		Model:      g.ResponseModel,
		Content:    content,
		StopReason: &toolUse,
		Usage:      usage,
	}, nil
}
