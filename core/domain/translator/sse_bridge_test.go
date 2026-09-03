package translator

import (
	"encoding/json"
	"reflect"
	"testing"

	"github.com/Bytonomics/cld-gateway/core/domain/dto"
	"github.com/Bytonomics/cld-gateway/core/domain/port/backend"
)

// rawEvent is a (event_name, data) pair mirroring one SSE frame from a
// backend_stream_*.sse fixture in
// crates/gateway-http-anthropic/tests/fixtures/streaming/.
type rawEvent struct {
	name string
	data string
}

func toBackendEvent(t *testing.T, ev rawEvent) backend.Event {
	t.Helper()
	return backend.Event{Type: ev.name, Data: json.RawMessage(ev.data)}
}

func runBridge(t *testing.T, tr *GenericBackendTranslator, events []rawEvent) []dto.SSEEvent {
	t.Helper()
	var out []dto.SSEEvent
	for _, ev := range events {
		mapped, err := tr.TranslateResponseEvent(toBackendEvent(t, ev))
		if err != nil {
			t.Fatalf("TranslateResponseEvent(%s): %v", ev.name, err)
		}
		out = append(out, mapped...)
	}
	return out
}

// expected is a (event, data) pair mirroring one line of an
// expected_anthropic_*.jsonl fixture, minus its leading message_start line:
// TranslateResponseEvent only ever sees one backend event at a time and has
// no notion of "this is the first one", so message_start is built by
// BuildStreamStartEvents (see its doc comment) and sent by the caller
// (message_service.go's runStream) before this bridge is ever invoked, not
// reproduced by anything under test here.
type expected struct {
	event string
	data  string
}

func assertSSEEventsEqual(t *testing.T, got []dto.SSEEvent, want []expected) {
	t.Helper()
	if len(got) != len(want) {
		t.Fatalf("event count = %d, want %d\ngot: %+v", len(got), len(want), describeEvents(got))
	}
	for i := range want {
		if got[i].Event != want[i].event {
			t.Errorf("event[%d].Event = %q, want %q", i, got[i].Event, want[i].event)
			continue
		}
		var gotVal, wantVal any
		if err := json.Unmarshal(got[i].Data, &gotVal); err != nil {
			t.Fatalf("event[%d] data not JSON: %v (%s)", i, err, got[i].Data)
		}
		if err := json.Unmarshal([]byte(want[i].data), &wantVal); err != nil {
			t.Fatalf("event[%d] expected data not JSON: %v", i, err)
		}
		if !reflect.DeepEqual(gotVal, wantVal) {
			t.Errorf("event[%d] data mismatch\n got:  %s\n want: %s", i, got[i].Data, want[i].data)
		}
	}
}

func describeEvents(events []dto.SSEEvent) string {
	out := ""
	for _, e := range events {
		out += e.Event + ": " + string(e.Data) + "\n"
	}
	return out
}

func newBridgeTranslator() *GenericBackendTranslator {
	return &GenericBackendTranslator{}
}

// TestMessageDeltaIncludesCumulativeUsage ports
// sse_bridge.rs message_delta_includes_cumulative_usage.
func TestMessageDeltaIncludesCumulativeUsage(t *testing.T) {
	usage := tokenUsage{
		InputTokens:           7,
		CachedInputTokens:     3,
		OutputTokens:          9,
		ReasoningOutputTokens: 2,
		TotalTokens:           16,
		WebSearchRequests:     2,
	}
	payload := anthropicUsageValue(usage)
	if payload["input_tokens"] != int64(4) {
		t.Errorf("input_tokens = %v, want 4", payload["input_tokens"])
	}
	if payload["cache_creation_input_tokens"] != 0 {
		t.Errorf("cache_creation_input_tokens = %v, want 0", payload["cache_creation_input_tokens"])
	}
	if payload["cache_read_input_tokens"] != int64(3) {
		t.Errorf("cache_read_input_tokens = %v, want 3", payload["cache_read_input_tokens"])
	}
	if payload["output_tokens"] != int64(9) {
		t.Errorf("output_tokens = %v, want 9", payload["output_tokens"])
	}
	serverToolUse, _ := payload["server_tool_use"].(map[string]any)
	if serverToolUse["web_search_requests"] != int64(2) {
		t.Errorf("web_search_requests = %v, want 2", serverToolUse["web_search_requests"])
	}
	if serverToolUse["web_fetch_requests"] != 0 {
		t.Errorf("web_fetch_requests = %v, want 0", serverToolUse["web_fetch_requests"])
	}
}

// TestCompletedUsagePreservesUpstreamCumulativeInputTokens ports
// sse_bridge.rs completed_usage_preserves_upstream_cumulative_input_tokens.
func TestCompletedUsagePreservesUpstreamCumulativeInputTokens(t *testing.T) {
	st := newBridgeState(nil, nil)
	handleCompleted(st, `{"type":"response.completed","response":{"usage":{"input_tokens":135,"input_tokens_details":{"cached_tokens":120},"output_tokens":7,"total_tokens":142}}}`)

	usage := st.completedUsage
	if usage == nil {
		t.Fatal("expected completed usage")
	}
	if usage.InputTokens != 135 {
		t.Errorf("InputTokens = %d, want 135", usage.InputTokens)
	}
	if usage.CachedInputTokens != 120 {
		t.Errorf("CachedInputTokens = %d, want 120", usage.CachedInputTokens)
	}
	if usage.OutputTokens != 7 {
		t.Errorf("OutputTokens = %d, want 7", usage.OutputTokens)
	}
	if usage.TotalTokens != 142 {
		t.Errorf("TotalTokens = %d, want 142", usage.TotalTokens)
	}
}

// TestStreamingBridgeMatchesTextOnlyFixture ports
// sse_bridge.rs streaming_bridge_matches_text_only_fixture, using the same
// event/expected-jsonl fixtures (minus the caller-built message_start
// line) from
// crates/gateway-http-anthropic/tests/fixtures/streaming/backend_stream_text_only.sse
// and .../expected_anthropic_text_only.jsonl.
func TestStreamingBridgeMatchesTextOnlyFixture(t *testing.T) {
	events := []rawEvent{
		{"response.output_text.delta", `{"type":"response.output_text.delta","delta":"Hello"}`},
		{"response.output_text.delta", `{"type":"response.output_text.delta","delta":" world"}`},
		{"response.completed", `{"type":"response.completed","response":{"id":"resp_1","usage":{"input_tokens":7,"output_tokens":9}}}`},
	}
	want := []expected{
		{"content_block_start", `{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}`},
		{"content_block_delta", `{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}`},
		{"content_block_delta", `{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":" world"}}`},
		{"content_block_stop", `{"type":"content_block_stop","index":0}`},
		{"message_delta", `{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"input_tokens":7,"output_tokens":9}}`},
		{"message_stop", `{"type":"message_stop"}`},
	}
	got := runBridge(t, newBridgeTranslator(), events)
	assertSSEEventsEqual(t, got, want)
}

// TestStreamingBridgeCleansStructuredOutputOptionalNulls ports
// sse_bridge.rs streaming_bridge_cleans_structured_output_optional_nulls.
func TestStreamingBridgeCleansStructuredOutputOptionalNulls(t *testing.T) {
	schema := map[string]any{
		"type": "object",
		"properties": map[string]any{
			"ok":         map[string]any{"type": "boolean"},
			"reason":     map[string]any{"type": "string"},
			"impossible": map[string]any{"type": "boolean"},
		},
		"required": []any{"ok", "reason"},
	}
	events := []rawEvent{
		{"response.output_text.delta", `{"type":"response.output_text.delta","delta":"{\"ok\":true,\"reason\":\"continuing\",\"impossible\":null}"}`},
		{"response.completed", `{"type":"response.completed","response":{"usage":{"input_tokens":7,"output_tokens":9,"total_tokens":16}}}`},
	}
	tr := &GenericBackendTranslator{StructuredOutputSchema: schema}
	got := runBridge(t, tr, events)

	var textDelta string
	found := false
	for _, ev := range got {
		if ev.Event != "content_block_delta" {
			continue
		}
		var v map[string]any
		if err := json.Unmarshal(ev.Data, &v); err != nil {
			t.Fatalf("unmarshal delta: %v", err)
		}
		delta, _ := v["delta"].(map[string]any)
		if text, ok := delta["text"].(string); ok {
			textDelta = text
			found = true
		}
	}
	if !found {
		t.Fatal("expected a text delta")
	}
	want := `{"ok":true,"reason":"continuing"}`
	if textDelta != want {
		t.Errorf("text delta = %q, want %q", textDelta, want)
	}
}

// TestStreamingBridgeMatchesToolCallFixtureAndSanitizesArgs ports
// sse_bridge.rs streaming_bridge_matches_tool_call_fixture_and_sanitizes_args.
func TestStreamingBridgeMatchesToolCallFixtureAndSanitizesArgs(t *testing.T) {
	events := []rawEvent{
		{"response.output_item.added", `{"type":"response.output_item.added","item":{"type":"function_call","call_id":"call_1","name":"Read","arguments":""}}`},
		{"response.function_call_arguments.delta", `{"type":"response.function_call_arguments.delta","call_id":"call_1","delta":"{\"file_path\":\"/tmp/a.txt\",\"pages\":\"\"}"}`},
		{"response.output_item.done", `{"type":"response.output_item.done","item":{"type":"function_call","call_id":"call_1","name":"Read","arguments":"{\"file_path\":\"/tmp/a.txt\",\"pages\":\"\"}"}}`},
		{"response.completed", `{"type":"response.completed","response":{"id":"resp_2","usage":{"input_tokens":11,"output_tokens":0}}}`},
	}
	want := []expected{
		{"content_block_start", `{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"call_1","name":"Read","input":{}}}`},
		{"content_block_delta", `{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"file_path\":\"/tmp/a.txt\"}"}}`},
		{"content_block_stop", `{"type":"content_block_stop","index":0}`},
		{"message_delta", `{"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"input_tokens":11,"output_tokens":0}}`},
		{"message_stop", `{"type":"message_stop"}`},
	}
	got := runBridge(t, newBridgeTranslator(), events)
	assertSSEEventsEqual(t, got, want)
}

// TestStreamingBridgeMatchesCustomToolCallFixture ports
// sse_bridge.rs streaming_bridge_matches_custom_tool_call_fixture.
func TestStreamingBridgeMatchesCustomToolCallFixture(t *testing.T) {
	events := []rawEvent{
		{"response.output_item.added", `{"type":"response.output_item.added","item":{"type":"custom_tool_call","call_id":"call_custom","name":"apply_patch","input":"","status":"in_progress"}}`},
		{"response.custom_tool_call_input.delta", `{"type":"response.custom_tool_call_input.delta","call_id":"call_custom","delta":"*** Begin Patch\n"}`},
		{"response.custom_tool_call_input.delta", `{"type":"response.custom_tool_call_input.delta","call_id":"call_custom","delta":"*** End Patch\n"}`},
		{"response.output_item.done", `{"type":"response.output_item.done","item":{"type":"custom_tool_call","call_id":"call_custom","name":"apply_patch","input":"*** Begin Patch\n*** End Patch\n"}}`},
		{"response.completed", `{"type":"response.completed","response":{"id":"resp_custom","usage":{"input_tokens":11,"output_tokens":0}}}`},
	}
	want := []expected{
		{"content_block_start", `{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"call_custom","name":"apply_patch","input":{}}}`},
		{"content_block_delta", `{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"input\":\"*** Begin Patch\\n*** End Patch\\n\"}"}}`},
		{"content_block_stop", `{"type":"content_block_stop","index":0}`},
		{"message_delta", `{"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"input_tokens":11,"output_tokens":0}}`},
		{"message_stop", `{"type":"message_stop"}`},
	}
	got := runBridge(t, newBridgeTranslator(), events)
	assertSSEEventsEqual(t, got, want)
}

// TestStreamingBridgeMatchesToolSearchCallFixture ports
// sse_bridge.rs streaming_bridge_matches_tool_search_call_fixture.
func TestStreamingBridgeMatchesToolSearchCallFixture(t *testing.T) {
	events := []rawEvent{
		{"response.output_item.done", `{"type":"response.output_item.done","item":{"type":"tool_search_call","call_id":"call_search","execution":"client","arguments":{"query":"Read"}}}`},
		{"response.completed", `{"type":"response.completed","response":{"id":"resp_search","usage":{"input_tokens":7,"output_tokens":0}}}`},
	}
	want := []expected{
		{"content_block_start", `{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"call_search","name":"tool_search","input":{}}}`},
		{"content_block_delta", `{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"query\":\"Read\"}"}}`},
		{"content_block_stop", `{"type":"content_block_stop","index":0}`},
		{"message_delta", `{"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"input_tokens":7,"output_tokens":0}}`},
		{"message_stop", `{"type":"message_stop"}`},
	}
	got := runBridge(t, newBridgeTranslator(), events)
	assertSSEEventsEqual(t, got, want)
}

// TestStreamingBridgeMatchesLocalShellCallFixture ports
// sse_bridge.rs streaming_bridge_matches_local_shell_call_fixture.
func TestStreamingBridgeMatchesLocalShellCallFixture(t *testing.T) {
	events := []rawEvent{
		{"response.output_item.done", `{"type":"response.output_item.done","item":{"type":"local_shell_call","call_id":"call_shell","status":"completed","action":{"type":"exec","command":["echo","hi"]}}}`},
		{"response.completed", `{"type":"response.completed","response":{"id":"resp_shell","usage":{"input_tokens":7,"output_tokens":0}}}`},
	}
	want := []expected{
		{"content_block_start", `{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"call_shell","name":"local_shell","input":{}}}`},
		{"content_block_delta", `{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"action\":{\"command\":[\"echo\",\"hi\"],\"type\":\"exec\"},\"status\":\"completed\"}"}}`},
		{"content_block_stop", `{"type":"content_block_stop","index":0}`},
		{"message_delta", `{"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"input_tokens":7,"output_tokens":0}}`},
		{"message_stop", `{"type":"message_stop"}`},
	}
	got := runBridge(t, newBridgeTranslator(), events)
	assertSSEEventsEqual(t, got, want)
}

// TestStreamingBridgeCountsHostedWebSearchUsage ports
// sse_bridge.rs streaming_bridge_counts_hosted_web_search_usage.
func TestStreamingBridgeCountsHostedWebSearchUsage(t *testing.T) {
	events := []rawEvent{
		{"response.output_item.done", `{"type":"response.output_item.done","item":{"type":"web_search_call","id":"ws_1","status":"completed","action":{"type":"search","query":"rust release","sources":[{"title":"Rust","url":"https://www.rust-lang.org"}]}}}`},
		{"response.output_text.delta", `{"delta":"Search complete."}`},
		{"response.completed", `{"type":"response.completed","response":{"output":[{"type":"web_search_call","id":"ws_1","status":"completed"}],"usage":{"input_tokens":1,"output_tokens":2,"total_tokens":3}}}`},
	}
	got := runBridge(t, newBridgeTranslator(), events)

	serverToolUses := 0
	webSearchResultsCount := 0
	var resultBlock map[string]any
	var messageDeltaPayload map[string]any
	for _, ev := range got {
		var v map[string]any
		if err := json.Unmarshal(ev.Data, &v); err != nil {
			t.Fatalf("unmarshal: %v", err)
		}
		if ev.Event == "content_block_start" {
			cb, _ := v["content_block"].(map[string]any)
			switch cb["type"] {
			case "server_tool_use":
				serverToolUses++
			case "web_search_tool_result":
				webSearchResultsCount++
				resultBlock = v
			}
		}
		if ev.Event == "message_delta" {
			messageDeltaPayload = v
		}
	}
	if serverToolUses != 1 {
		t.Errorf("server_tool_use blocks = %d, want 1", serverToolUses)
	}
	if webSearchResultsCount != 1 {
		t.Errorf("web_search_tool_result blocks = %d, want 1", webSearchResultsCount)
	}
	if resultBlock == nil {
		t.Fatal("expected a web_search_tool_result block")
	}
	content, _ := resultBlock["content_block"].(map[string]any)
	results, _ := content["content"].([]any)
	if len(results) == 0 {
		t.Fatal("expected results content")
	}
	first, _ := results[0].(map[string]any)
	if first["url"] != "https://www.rust-lang.org" {
		t.Errorf("result url = %v, want https://www.rust-lang.org", first["url"])
	}
	if messageDeltaPayload == nil {
		t.Fatal("expected a message_delta event")
	}
	usage, _ := messageDeltaPayload["usage"].(map[string]any)
	serverToolUse, _ := usage["server_tool_use"].(map[string]any)
	if serverToolUse["web_search_requests"] != float64(1) {
		t.Errorf("web_search_requests = %v, want 1", serverToolUse["web_search_requests"])
	}
	if serverToolUse["web_fetch_requests"] != float64(0) {
		t.Errorf("web_fetch_requests = %v, want 0", serverToolUse["web_fetch_requests"])
	}
}

// TestStreamingBridgeSurfacesBackendFailureEvent ports
// sse_bridge.rs streaming_bridge_surfaces_backend_failure_event.
func TestStreamingBridgeSurfacesBackendFailureEvent(t *testing.T) {
	events := []rawEvent{
		{"response.created", `{"type":"response.created"}`},
		{"response.in_progress", `{"type":"response.in_progress"}`},
		{"error", `{"type":"error","message":"model unavailable"}`},
		{"response.failed", `{"type":"response.failed"}`},
	}
	got := runBridge(t, newBridgeTranslator(), events)

	var errPayload map[string]any
	for _, ev := range got {
		if ev.Event != "error" {
			continue
		}
		if err := json.Unmarshal(ev.Data, &errPayload); err != nil {
			t.Fatalf("unmarshal error event: %v", err)
		}
	}
	if errPayload == nil {
		t.Fatal("expected an error event")
	}
	errObj, _ := errPayload["error"].(map[string]any)
	want := "backend stream failed: error: model unavailable"
	if errObj["message"] != want {
		t.Errorf("error message = %v, want %q", errObj["message"], want)
	}
}

// TestBuildUnaryResponseNoToolCalls covers the tool-call-free branch of
// build_unary_messages_response (lib.rs:1580-1591): plain text response,
// end_turn stop reason, no cache/server_tool_use fields.
func TestBuildUnaryResponseNoToolCalls(t *testing.T) {
	tr := &GenericBackendTranslator{ResponseModel: "gpt-5.6-sol"}
	events := []backend.Event{
		toBackendEvent(t, rawEvent{"response.output_text.delta", `{"type":"response.output_text.delta","delta":"Hello"}`}),
		toBackendEvent(t, rawEvent{"response.completed", `{"type":"response.completed","response":{"usage":{"input_tokens":7,"output_tokens":9}}}`}),
	}
	resp, err := tr.BuildUnaryResponse(events)
	if err != nil {
		t.Fatalf("BuildUnaryResponse: %v", err)
	}
	if resp.Model != "gpt-5.6-sol" {
		t.Errorf("Model = %q, want gpt-5.6-sol", resp.Model)
	}
	if resp.StopReason == nil || *resp.StopReason != "end_turn" {
		t.Errorf("StopReason = %v, want end_turn", resp.StopReason)
	}
	if len(resp.Content) != 1 || resp.Content[0].BlockType != "text" || resp.Content[0].Text == nil || *resp.Content[0].Text != "Hello" {
		t.Errorf("Content = %+v, want single text block \"Hello\"", resp.Content)
	}
	if resp.Usage.InputTokens != 7 || resp.Usage.OutputTokens != 9 {
		t.Errorf("Usage = %+v, want input=7 output=9", resp.Usage)
	}
	if resp.Usage.ServerToolUse != nil {
		t.Errorf("ServerToolUse = %+v, want nil", resp.Usage.ServerToolUse)
	}
}

// TestBuildUnaryResponseWithToolCall covers the tool-call branch of
// build_unary_messages_response / tool_call_content_block
// (lib.rs:1592-1636): tool_use stop reason, sanitized tool_use content
// block.
func TestBuildUnaryResponseWithToolCall(t *testing.T) {
	tr := &GenericBackendTranslator{ResponseModel: "gpt-5.6-sol"}
	events := []backend.Event{
		toBackendEvent(t, rawEvent{"response.output_item.done", `{"type":"response.output_item.done","item":{"type":"function_call","call_id":"call_1","name":"Read","arguments":"{\"file_path\":\"/tmp/a.txt\",\"pages\":\"\"}"}}`}),
		toBackendEvent(t, rawEvent{"response.completed", `{"type":"response.completed","response":{"usage":{"input_tokens":11,"output_tokens":0}}}`}),
	}
	resp, err := tr.BuildUnaryResponse(events)
	if err != nil {
		t.Fatalf("BuildUnaryResponse: %v", err)
	}
	if resp.StopReason == nil || *resp.StopReason != "tool_use" {
		t.Errorf("StopReason = %v, want tool_use", resp.StopReason)
	}
	if len(resp.Content) != 1 {
		t.Fatalf("Content = %+v, want a single tool_use block", resp.Content)
	}
	block := resp.Content[0]
	if block.BlockType != "tool_use" || block.ID == nil || *block.ID != "call_1" || block.Name == nil || *block.Name != "Read" {
		t.Errorf("block = %+v, want tool_use/call_1/Read", block)
	}
	input, _ := block.Input.(map[string]any)
	if _, hasPages := input["pages"]; hasPages {
		t.Errorf("input = %+v, want pages sanitized away", input)
	}
	if input["file_path"] != "/tmp/a.txt" {
		t.Errorf("input.file_path = %v, want /tmp/a.txt", input["file_path"])
	}
}

func TestBuildStreamStartEventsShapeAndContent(t *testing.T) {
	events := BuildStreamStartEvents("msg_abc123", "claude-opus-4-6", nil)
	assertSSEEventsEqual(t, events, []expected{{
		event: "message_start",
		data: `{
			"type": "message_start",
			"message": {
				"id": "msg_abc123",
				"type": "message",
				"role": "assistant",
				"content": [],
				"model": "claude-opus-4-6",
				"stop_reason": null,
				"stop_sequence": null,
				"usage": {
					"input_tokens": 0,
					"cache_creation_input_tokens": 0,
					"cache_read_input_tokens": 0,
					"output_tokens": 0
				}
			}
		}`,
	}})
}

func TestBuildStreamStartEventsIsSingleEvent(t *testing.T) {
	events := BuildStreamStartEvents("msg_x", "model_y", nil)
	if len(events) != 1 {
		t.Fatalf("BuildStreamStartEvents returned %d events, want exactly 1", len(events))
	}
	if events[0].Event != "message_start" {
		t.Errorf("Event = %q, want message_start", events[0].Event)
	}
}

func TestBuildStreamStartEventsWithWarnings(t *testing.T) {
	warnings := []dto.Warning{{Code: "delta_calculation_skipped", Message: "[CLD-Gateway] Sent full conversation history instead of an incremental update for this turn."}}
	events := BuildStreamStartEvents("msg_w1", "claude-opus-4-6", warnings)
	assertSSEEventsEqual(t, events, []expected{{
		event: "message_start",
		data: `{
			"type": "message_start",
			"message": {
				"id": "msg_w1",
				"type": "message",
				"role": "assistant",
				"content": [],
				"model": "claude-opus-4-6",
				"stop_reason": null,
				"stop_sequence": null,
				"usage": {
					"input_tokens": 0,
					"cache_creation_input_tokens": 0,
					"cache_read_input_tokens": 0,
					"output_tokens": 0
				},
				"warnings": [{"code": "delta_calculation_skipped", "message": "[CLD-Gateway] Sent full conversation history instead of an incremental update for this turn."}]
			}
		}`,
	}})
}
