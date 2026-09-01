package translator

import (
	"context"
	"encoding/json"
	"strings"
	"testing"

	"github.com/Bytonomics/cld-gateway/config"
	"github.com/Bytonomics/cld-gateway/core/domain/dto"
	"github.com/Bytonomics/cld-gateway/core/domain/port/backend"
)

// mapToolCallKindLookup is a test-only ToolCallKindLookup, mirroring Rust's
// ToolTranslationContext::new(HashMap::from([(call_id, kind)])) test helper
// (translate.rs:794-796).
type mapToolCallKindLookup map[string]ToolCallKind

func (m mapToolCallKindLookup) ToolCallKind(callID string) (ToolCallKind, bool) {
	kind, ok := m[callID]
	return kind, ok
}

func newTestTranslator() *GenericBackendTranslator {
	return &GenericBackendTranslator{
		ClaudeCodeConfig: config.Default().Workflow.ClaudeCode,
	}
}

func newTestTranslatorWithContext(callID string, kind ToolCallKind) *GenericBackendTranslator {
	return &GenericBackendTranslator{
		ClaudeCodeConfig: config.Default().Workflow.ClaudeCode,
		ToolCallKinds:    mapToolCallKindLookup{callID: kind},
	}
}

func baseReq() *dto.MessagesRequest {
	return &dto.MessagesRequest{
		Model:    config.DefaultBackendModel,
		Messages: nil,
	}
}

func textMessage(role, text string) dto.Message {
	return dto.Message{Role: role, Content: dto.Content{Text: &text}}
}

func textBlock(text string) dto.ContentBlock {
	return dto.ContentBlock{BlockType: "text", Text: &text}
}

func mustTranslate(t *testing.T, req *dto.MessagesRequest) *backend.Request {
	t.Helper()
	tr := newTestTranslator()
	out, err := tr.TranslateRequest(context.Background(), req, TranslateMeta{Model: req.Model})
	if err != nil {
		t.Fatalf("TranslateRequest: %v", err)
	}
	return out
}

func mustTranslateWithTranslator(t *testing.T, tr *GenericBackendTranslator, req *dto.MessagesRequest) *backend.Request {
	t.Helper()
	out, err := tr.TranslateRequest(context.Background(), req, TranslateMeta{Model: req.Model})
	if err != nil {
		t.Fatalf("TranslateRequest: %v", err)
	}
	return out
}

func serializedInput(t *testing.T, r *backend.Request) string {
	t.Helper()
	b, err := json.Marshal(r.Input)
	if err != nil {
		t.Fatalf("marshal input: %v", err)
	}
	return string(b)
}

func TestDefaultsInstructionsWhenSystemEmpty(t *testing.T) {
	translated := mustTranslate(t, baseReq())
	if translated.Instructions != "You are a helpful assistant." {
		t.Errorf("Instructions = %q, want default", translated.Instructions)
	}
}

func TestOutputConfigOptionalFieldsBecomeNullableRequiredForOpenAIStrictSchema(t *testing.T) {
	req := baseReq()
	req.OutputConfig = &dto.OutputConfig{
		Format: map[string]any{
			"type": "json_schema",
			"schema": map[string]any{
				"type": "object",
				"properties": map[string]any{
					"ok":         map[string]any{"type": "boolean"},
					"reason":     map[string]any{"type": "string"},
					"impossible": map[string]any{"type": "boolean"},
				},
				"required":             []any{"ok", "reason"},
				"additionalProperties": false,
			},
		},
	}

	translated := mustTranslate(t, req)
	if translated.Text == nil {
		t.Fatal("expected translated.Text")
	}
	format, _ := (*translated.Text)["format"].(map[string]any)
	schema, _ := format["schema"].(map[string]any)
	if schema == nil {
		t.Fatal("expected schema in translated output")
	}

	required, _ := schema["required"].([]any)
	gotRequired := []any{}
	gotRequired = append(gotRequired, required...)
	wantRequired := []any{"impossible", "ok", "reason"}
	if !equalAnySlices(gotRequired, wantRequired) {
		t.Errorf("required = %v, want %v", gotRequired, wantRequired)
	}

	properties, _ := schema["properties"].(map[string]any)
	impossible, _ := properties["impossible"].(map[string]any)
	if !equalAnySlices(toAnySlice(impossible["type"]), []any{"boolean", "null"}) {
		t.Errorf("impossible.type = %v, want [boolean null]", impossible["type"])
	}
	ok, _ := properties["ok"].(map[string]any)
	if ok["type"] != "boolean" {
		t.Errorf("ok.type = %v, want boolean", ok["type"])
	}
}

func TestOutputConfigNestedOptionalFieldsAreNullableRequiredRecursively(t *testing.T) {
	req := baseReq()
	req.OutputConfig = &dto.OutputConfig{
		Format: map[string]any{
			"type": "json_schema",
			"schema": map[string]any{
				"type": "object",
				"properties": map[string]any{
					"outer": map[string]any{
						"type": "object",
						"properties": map[string]any{
							"required_child": map[string]any{"type": "string"},
							"optional_child": map[string]any{"type": "integer"},
						},
						"required": []any{"required_child"},
					},
				},
				"required": []any{"outer"},
			},
		},
	}

	translated := mustTranslate(t, req)
	format, _ := (*translated.Text)["format"].(map[string]any)
	schema, _ := format["schema"].(map[string]any)
	properties, _ := schema["properties"].(map[string]any)
	outer, _ := properties["outer"].(map[string]any)

	if outer["additionalProperties"] != false {
		t.Errorf("outer.additionalProperties = %v, want false", outer["additionalProperties"])
	}
	if !equalAnySlices(toAnySlice(outer["required"]), []any{"optional_child", "required_child"}) {
		t.Errorf("outer.required = %v, want [optional_child required_child]", outer["required"])
	}
	outerProps, _ := outer["properties"].(map[string]any)
	optionalChild, _ := outerProps["optional_child"].(map[string]any)
	if !equalAnySlices(toAnySlice(optionalChild["type"]), []any{"integer", "null"}) {
		t.Errorf("optional_child.type = %v, want [integer null]", optionalChild["type"])
	}
}

func TestLatestUserMessageGetsPriorityDirective(t *testing.T) {
	req := baseReq()
	req.Messages = []dto.Message{textMessage("user", "explain the current diff")}

	translated := mustTranslate(t, req)
	if !strings.HasPrefix(translated.Instructions, "Follow the prompt coming with this instruction") {
		t.Errorf("Instructions = %q, want priority directive prefix", translated.Instructions)
	}
	if !strings.Contains(serializedInput(t, translated), "explain the current diff") {
		t.Error("expected input to contain user text")
	}
}

func TestTurnLevelSystemMessagesArePromotedToInstructions(t *testing.T) {
	req := baseReq()
	req.System = []dto.SystemBlock{{BlockType: "text", Text: strPtr("Top-level system prompt.")}}
	req.Messages = []dto.Message{
		textMessage("system", "Turn-level system prompt."),
		textMessage("user", "Handle the current task."),
	}

	translated := mustTranslate(t, req)
	input := serializedInput(t, translated)

	if !strings.Contains(translated.Instructions, "Top-level system prompt.") {
		t.Error("expected instructions to contain top-level system prompt")
	}
	if !strings.Contains(translated.Instructions, "Turn-level system prompt.") {
		t.Error("expected instructions to contain turn-level system prompt")
	}
	if !strings.Contains(input, "Handle the current task.") {
		t.Error("expected input to contain the user message")
	}
	if strings.Contains(input, `"role":"system"`) {
		t.Error("expected no system-role item in input")
	}
	if strings.Contains(input, "Turn-level system prompt.") {
		t.Error("expected turn-level system text removed from input")
	}
}

func TestTurnLevelSystemBlockTextIsPromotedAndRemovedFromInput(t *testing.T) {
	req := baseReq()
	req.Messages = []dto.Message{
		{Role: "system", Content: dto.Content{Blocks: []dto.ContentBlock{
			textBlock("First turn-level system block."),
			textBlock("Second turn-level system block."),
		}}},
	}

	translated := mustTranslate(t, req)
	input := serializedInput(t, translated)

	if !strings.Contains(translated.Instructions, "First turn-level system block.\n\nSecond turn-level system block.") {
		t.Errorf("Instructions = %q, want joined block text", translated.Instructions)
	}
	if strings.Contains(input, `"role":"system"`) {
		t.Error("expected no system-role item in input")
	}
	if strings.Contains(strings.ToLower(input), "turn-level system block") {
		t.Error("expected turn-level system block text removed from input")
	}
}

func TestTranslatesBase64ImageToDataURLInputImage(t *testing.T) {
	req := baseReq()
	req.Messages = []dto.Message{
		{Role: "user", Content: dto.Content{Blocks: []dto.ContentBlock{
			{
				BlockType: "image",
				Source: &dto.ImageSource{
					SourceType: "base64",
					MediaType:  strPtr("image/png"),
					Data:       strPtr("AAA="),
				},
			},
		}}},
	}

	translated := mustTranslate(t, req)
	var msg map[string]any
	for _, item := range translated.Input {
		if item["type"] == "message" {
			msg = item
			break
		}
	}
	if msg == nil {
		t.Fatal("expected a message item")
	}
	content, _ := msg["content"].([]any)
	if len(content) == 0 {
		t.Fatal("expected message content")
	}
	content0, _ := content[0].(map[string]any)
	if content0["type"] != "input_image" {
		t.Errorf("content0.type = %v, want input_image", content0["type"])
	}
	if content0["image_url"] != "data:image/png;base64,AAA=" {
		t.Errorf("content0.image_url = %v, want data URL", content0["image_url"])
	}
}

func TestToolResultOutputIsWireTextString(t *testing.T) {
	req := baseReq()
	req.Messages = []dto.Message{
		{Role: "user", Content: dto.Content{Blocks: []dto.ContentBlock{
			{
				BlockType: "tool_result",
				ToolUseID: strPtr("call_123"),
				Content:   []any{map[string]any{"type": "text", "text": "ok"}},
				IsError:   boolPtr(false),
			},
		}}},
	}

	translated := mustTranslate(t, req)
	var item map[string]any
	for _, it := range translated.Input {
		if it["type"] == "function_call_output" {
			item = it
			break
		}
	}
	if item == nil {
		t.Fatal("expected a function_call_output item")
	}
	if item["call_id"] != "call_123" {
		t.Errorf("call_id = %v, want call_123", item["call_id"])
	}
	output, _ := item["output"].([]any)
	if len(output) == 0 {
		t.Fatal("expected output content")
	}
	first, _ := output[0].(map[string]any)
	if first["type"] != "input_text" {
		t.Errorf("output[0].type = %v, want input_text", first["type"])
	}
}

func TestToolDefinitionsTranslateToBackendTools(t *testing.T) {
	req := baseReq()
	req.System = []dto.SystemBlock{{BlockType: "text", Text: strPtr("You are a helpful assistant.")}}
	req.Messages = []dto.Message{textMessage("user", "hi")}
	req.Tools = []dto.Tool{{
		Name:        "Read",
		Description: strPtr("Read a file from disk"),
		InputSchema: map[string]any{
			"type":                 "object",
			"additionalProperties": false,
			"properties": map[string]any{
				"file_path": map[string]any{"type": "string"},
				"offset":    map[string]any{"type": "integer"},
				"limit":     map[string]any{"type": "integer"},
				"pages":     map[string]any{"type": "string"},
			},
			"required": []any{"file_path"},
		},
	}}

	translated := mustTranslate(t, req)
	if len(translated.Tools) != 1 {
		t.Fatalf("len(Tools) = %d, want 1", len(translated.Tools))
	}
	tool := translated.Tools[0]
	if tool["type"] != "function" {
		t.Errorf("type = %v, want function", tool["type"])
	}
	if tool["name"] != "Read" {
		t.Errorf("name = %v, want Read", tool["name"])
	}
	params, _ := tool["parameters"].(map[string]any)
	if params["type"] != "object" {
		t.Errorf("parameters.type = %v, want object", params["type"])
	}
	if params["additionalProperties"] != false {
		t.Errorf("parameters.additionalProperties = %v, want false", params["additionalProperties"])
	}
}

func TestReadToolSchemaEnforcesDecimalIntegerLineNumbers(t *testing.T) {
	req := baseReq()
	req.Tools = []dto.Tool{{
		Name:        "Read",
		Description: strPtr("Read a file from disk"),
		InputSchema: map[string]any{
			"type":                 "object",
			"additionalProperties": false,
			"properties": map[string]any{
				"file_path": map[string]any{"type": "string"},
				"offset":    map[string]any{"type": "number", "description": "Line offset"},
				"limit":     map[string]any{"type": "number", "description": "Line count"},
			},
			"required": []any{"file_path"},
		},
	}}

	translated := mustTranslate(t, req)
	parameters := translated.Tools[0]["parameters"].(map[string]any)
	properties := parameters["properties"].(map[string]any)
	offset := properties["offset"].(map[string]any)
	limit := properties["limit"].(map[string]any)

	if offset["type"] != "integer" || limit["type"] != "integer" {
		t.Errorf("offset.type=%v limit.type=%v, want integer", offset["type"], limit["type"])
	}
	if offset["minimum"] != 1 || limit["minimum"] != 1 {
		t.Errorf("offset.minimum=%v limit.minimum=%v, want 1", offset["minimum"], limit["minimum"])
	}
	offsetDesc, _ := offset["description"].(string)
	if !strings.Contains(offsetDesc, "base-10 decimal digits") || !strings.Contains(offsetDesc, "scientific notation") {
		t.Errorf("offset.description = %q, missing expected phrases", offsetDesc)
	}
	if !strings.Contains(translated.Instructions, "When calling Read, offset and limit must be JSON whole-number integers") {
		t.Error("expected Read directive in instructions")
	}
}

func TestAgentToolSchemaHidesIsolationFromBackend(t *testing.T) {
	req := baseReq()
	req.Tools = []dto.Tool{{
		Name:        "Agent",
		Description: strPtr("Launch a subagent"),
		InputSchema: map[string]any{
			"type":                 "object",
			"additionalProperties": false,
			"properties": map[string]any{
				"description": map[string]any{"type": "string"},
				"prompt":      map[string]any{"type": "string"},
				"isolation":   map[string]any{"type": "string", "enum": []any{"worktree"}},
			},
			"required": []any{"description", "prompt", "isolation"},
		},
	}}

	translated := mustTranslate(t, req)
	parameters := translated.Tools[0]["parameters"].(map[string]any)
	properties := parameters["properties"].(map[string]any)
	required := parameters["required"].([]any)

	if _, ok := properties["isolation"]; ok {
		t.Error("expected isolation removed from properties")
	}
	for _, f := range required {
		if f == "isolation" {
			t.Error("expected isolation removed from required")
		}
	}
}

func TestHostedWebSearchTranslatesToOpenAIWebSearch(t *testing.T) {
	req := baseReq()
	req.Tools = []dto.Tool{{
		Name:           "web_search",
		ToolType:       strPtr("web_search_20250305"),
		AllowedDomains: []string{"github.com", "docs.brew.sh"},
		MaxUses:        uint32Ptr(8),
	}}

	translated := mustTranslate(t, req)
	if len(translated.Tools) != 1 {
		t.Fatalf("len(Tools) = %d, want 1", len(translated.Tools))
	}
	if translated.Tools[0]["type"] != "web_search" {
		t.Errorf("type = %v, want web_search", translated.Tools[0]["type"])
	}
	filters, _ := translated.Tools[0]["filters"].(map[string]any)
	allowed, _ := filters["allowed_domains"].([]string)
	if len(allowed) == 0 || allowed[0] != "github.com" {
		t.Errorf("filters.allowed_domains[0] = %v, want github.com", allowed)
	}
	if len(translated.Include) != 1 || translated.Include[0] != "web_search_call.action.sources" {
		t.Errorf("Include = %v, want [web_search_call.action.sources]", translated.Include)
	}
	if translated.ToolChoice != "required" {
		t.Errorf("ToolChoice = %v, want required", translated.ToolChoice)
	}
}

func TestHostedWebSearchRejectsBlockedDomains(t *testing.T) {
	tr := newTestTranslator()
	req := baseReq()
	req.Tools = []dto.Tool{{
		Name:           "web_search",
		ToolType:       strPtr("web_search_20250305"),
		BlockedDomains: []string{"example.com"},
		MaxUses:        uint32Ptr(8),
	}}

	_, err := tr.TranslateRequest(context.Background(), req, TranslateMeta{Model: req.Model})
	if err == nil {
		t.Fatal("expected error for blocked_domains")
	}
	if !strings.Contains(err.Error(), "blocked_domains") || !strings.Contains(err.Error(), "unsupported") {
		t.Errorf("error = %v, missing expected phrases", err)
	}
}

func TestHostedWebSearchRejectsUnknownFields(t *testing.T) {
	tr := newTestTranslator()
	req := baseReq()
	req.Tools = []dto.Tool{{
		Name:     "web_search",
		ToolType: strPtr("web_search_20250305"),
		MaxUses:  uint32Ptr(8),
		Extra:    map[string]json.RawMessage{"mystery": json.RawMessage("true")},
	}}

	_, err := tr.TranslateRequest(context.Background(), req, TranslateMeta{Model: req.Model})
	if err == nil {
		t.Fatal("expected error for unknown fields")
	}
	if !strings.Contains(err.Error(), "unsupported field") || !strings.Contains(err.Error(), "mystery") {
		t.Errorf("error = %v, missing expected phrases", err)
	}
}

func TestToolResultRichFixturePreservesImageContentItems(t *testing.T) {
	req := baseReq()
	req.Messages = []dto.Message{
		{Role: "user", Content: dto.Content{Blocks: []dto.ContentBlock{
			{
				BlockType: "tool_result",
				ToolUseID: strPtr("call_123"),
				IsError:   boolPtr(false),
				Content: []any{
					map[string]any{"type": "text", "text": "ok"},
					map[string]any{
						"type": "image",
						"source": map[string]any{
							"type":       "base64",
							"media_type": "image/png",
							"data":       "AAA=",
						},
					},
				},
			},
		}}},
	}

	translated := mustTranslate(t, req)
	var item map[string]any
	for _, it := range translated.Input {
		if it["type"] == "function_call_output" {
			item = it
			break
		}
	}
	if item == nil {
		t.Fatal("expected function_call_output item")
	}
	output, _ := item["output"].([]any)
	found := false
	for _, v := range output {
		if m, ok := v.(map[string]any); ok && m["type"] == "input_image" {
			found = true
		}
	}
	if !found {
		t.Error("expected an input_image content item in tool_result output")
	}
}

func TestToolResultUsesCustomOutputTypeFromContext(t *testing.T) {
	tr := newTestTranslatorWithContext("call_custom", ToolCallKindCustom)
	req := baseReq()
	req.Messages = []dto.Message{
		{Role: "user", Content: dto.Content{Blocks: []dto.ContentBlock{
			{
				BlockType: "tool_result",
				ToolUseID: strPtr("call_custom"),
				Content:   "ok",
				IsError:   boolPtr(false),
			},
		}}},
	}

	translated := mustTranslateWithTranslator(t, tr, req)
	if len(translated.Input) == 0 {
		t.Fatal("expected an output item")
	}
	if translated.Input[0]["type"] != "custom_tool_call_output" {
		t.Errorf("type = %v, want custom_tool_call_output", translated.Input[0]["type"])
	}
}

func TestToolResultUsesToolSearchOutputTypeFromContext(t *testing.T) {
	tr := newTestTranslatorWithContext("call_search", ToolCallKindToolSearch)
	req := baseReq()
	req.Messages = []dto.Message{
		{Role: "user", Content: dto.Content{Blocks: []dto.ContentBlock{
			{
				BlockType: "tool_result",
				ToolUseID: strPtr("call_search"),
				Content: map[string]any{
					"tools": []any{map[string]any{"type": "function", "name": "Read"}},
				},
				IsError: boolPtr(false),
			},
		}}},
	}

	translated := mustTranslateWithTranslator(t, tr, req)
	if len(translated.Input) == 0 {
		t.Fatal("expected an output item")
	}
	item := translated.Input[0]
	if item["type"] != "tool_search_output" {
		t.Errorf("type = %v, want tool_search_output", item["type"])
	}
	tools, _ := item["tools"].([]any)
	if len(tools) != 1 {
		t.Errorf("len(tools) = %d, want 1", len(tools))
	}
}

func TestToolResultUsesFunctionOutputForLocalShellContext(t *testing.T) {
	tr := newTestTranslatorWithContext("call_shell", ToolCallKindLocalShell)
	req := baseReq()
	req.Messages = []dto.Message{
		{Role: "user", Content: dto.Content{Blocks: []dto.ContentBlock{
			{
				BlockType: "tool_result",
				ToolUseID: strPtr("call_shell"),
				Content:   "ok",
				IsError:   boolPtr(false),
			},
		}}},
	}

	translated := mustTranslateWithTranslator(t, tr, req)
	if len(translated.Input) == 0 {
		t.Fatal("expected an output item")
	}
	if translated.Input[0]["type"] != "function_call_output" {
		t.Errorf("type = %v, want function_call_output", translated.Input[0]["type"])
	}
}

func TestReplayedCustomToolUseUsesCustomCallTypeFromContext(t *testing.T) {
	tr := newTestTranslatorWithContext("call_custom", ToolCallKindCustom)
	req := baseReq()
	req.Messages = []dto.Message{
		{Role: "assistant", Content: dto.Content{Blocks: []dto.ContentBlock{
			{
				BlockType: "tool_use",
				ID:        strPtr("call_custom"),
				Name:      strPtr("apply_patch"),
				Input:     map[string]any{"input": "*** Begin Patch\n*** End Patch\n"},
			},
		}}},
	}

	translated := mustTranslateWithTranslator(t, tr, req)
	if len(translated.Input) == 0 {
		t.Fatal("expected a tool use item")
	}
	item := translated.Input[0]
	if item["type"] != "custom_tool_call" {
		t.Errorf("type = %v, want custom_tool_call", item["type"])
	}
	if item["input"] != "*** Begin Patch\n*** End Patch\n" {
		t.Errorf("input = %v, want patch text", item["input"])
	}
}

func strPtr(s string) *string    { return &s }
func boolPtr(b bool) *bool       { return &b }
func uint32Ptr(v uint32) *uint32 { return &v }

func toAnySlice(v any) []any {
	arr, ok := v.([]any)
	if !ok {
		return nil
	}
	return arr
}

func equalAnySlices(a, b []any) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range a {
		if a[i] != b[i] {
			return false
		}
	}
	return true
}
