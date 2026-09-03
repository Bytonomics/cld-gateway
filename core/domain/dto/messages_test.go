package dto

import (
	"encoding/json"
	"strings"
	"testing"

	"github.com/SmrutAI/pedantigo/v2/validator"
)

func TestMessagesRequest_ContentBlockArray(t *testing.T) {
	body := []byte(`{"model":"m","messages":[{"role":"user","content":[{"type":"text","text":"hi"}]}]}`)
	var req MessagesRequest
	if err := validator.UnmarshalInto(body, &req); err != nil {
		t.Fatalf("UnmarshalInto returned error: %v", err)
	}
	if len(req.Messages) != 1 {
		t.Fatalf("want 1 message, got %d", len(req.Messages))
	}
	blocks := req.Messages[0].Content.Blocks
	if len(blocks) != 1 {
		t.Fatalf("want 1 content block, got %d", len(blocks))
	}
	if blocks[0].BlockType != "text" {
		t.Errorf("BlockType = %q, want \"text\"", blocks[0].BlockType)
	}
	if blocks[0].Text == nil || *blocks[0].Text != "hi" {
		t.Errorf("block Text = %v, want \"hi\"", blocks[0].Text)
	}
	if req.Messages[0].Content.Text != nil {
		t.Errorf("Content.Text should be nil for array content")
	}
}

func TestMessagesRequest_ContentString(t *testing.T) {
	body := []byte(`{"model":"m","messages":[{"role":"user","content":"hello"}]}`)
	var req MessagesRequest
	if err := validator.UnmarshalInto(body, &req); err != nil {
		t.Fatalf("UnmarshalInto returned error: %v", err)
	}
	c := req.Messages[0].Content
	if c.Text == nil || *c.Text != "hello" {
		t.Errorf("Content.Text = %v, want \"hello\"", c.Text)
	}
	if c.Blocks != nil {
		t.Errorf("Content.Blocks should be nil for string content")
	}
}

func TestMessagesRequest_ContentBlockRequiredTypeEnforced(t *testing.T) {
	// A content block missing its required "type" must fail validation.
	body := []byte(`{"model":"m","messages":[{"role":"user","content":[{"text":"hi"}]}]}`)
	var req MessagesRequest
	if err := validator.UnmarshalInto(body, &req); err == nil {
		t.Fatalf("expected error for content block missing required type, got nil")
	}
}

func TestMessagesRequest_ToolForwardCompatExtra(t *testing.T) {
	// An unmodeled key on a tool must be preserved in Extra (forward-compat).
	body := []byte(`{"model":"m","messages":[{"role":"user","content":"x"}],"tools":[{"name":"t","future_field":{"a":1}}]}`)
	var req MessagesRequest
	if err := validator.UnmarshalInto(body, &req); err != nil {
		t.Fatalf("UnmarshalInto returned error: %v", err)
	}
	if len(req.Tools) != 1 {
		t.Fatalf("want 1 tool, got %d", len(req.Tools))
	}
	if req.Tools[0].Name != "t" {
		t.Errorf("tool Name = %q, want \"t\"", req.Tools[0].Name)
	}
	if _, ok := req.Tools[0].Extra["future_field"]; !ok {
		t.Errorf("unmodeled key future_field not captured in Tool.Extra: %v", req.Tools[0].Extra)
	}
}

func TestMessagesRequest_ToolRequiredNameEnforced(t *testing.T) {
	// A tool missing its required "name" must fail validation.
	body := []byte(`{"model":"m","messages":[{"role":"user","content":"x"}],"tools":[{"description":"d"}]}`)
	var req MessagesRequest
	if err := validator.UnmarshalInto(body, &req); err == nil {
		t.Fatalf("expected error for tool missing required name, got nil")
	}
}

func TestMessagesResponse_NoWarnings_OmitsWarningsKey(t *testing.T) {
	resp := MessagesResponse{ID: "msg_1", Type: "message", Role: "assistant", Model: "claude-opus-4-6", Content: []ContentBlock{}, Usage: Usage{}}
	data, err := json.Marshal(resp)
	if err != nil {
		t.Fatalf("Marshal failed: %v", err)
	}
	if strings.Contains(string(data), "warnings") {
		t.Errorf("marshaled JSON contains \"warnings\" key when Warnings is nil/empty: %s", data)
	}
}

func TestMessagesResponse_WithWarnings_MarshalsCorrectly(t *testing.T) {
	resp := MessagesResponse{
		ID: "msg_1", Type: "message", Role: "assistant", Model: "claude-opus-4-6",
		Content: []ContentBlock{}, Usage: Usage{},
		Warnings: []Warning{{Code: "delta_calculation_skipped", Message: "[CLD-Gateway] Sent full conversation history instead of an incremental update for this turn."}},
	}
	data, err := json.Marshal(resp)
	if err != nil {
		t.Fatalf("Marshal failed: %v", err)
	}
	var roundTripped MessagesResponse
	if err := json.Unmarshal(data, &roundTripped); err != nil {
		t.Fatalf("Unmarshal failed: %v", err)
	}
	if len(roundTripped.Warnings) != 1 {
		t.Fatalf("len(Warnings) = %d, want 1", len(roundTripped.Warnings))
	}
	if roundTripped.Warnings[0].Code != "delta_calculation_skipped" {
		t.Errorf("Warnings[0].Code = %q, want %q", roundTripped.Warnings[0].Code, "delta_calculation_skipped")
	}
	wantMsg := "[CLD-Gateway] Sent full conversation history instead of an incremental update for this turn."
	if roundTripped.Warnings[0].Message != wantMsg {
		t.Errorf("Warnings[0].Message = %q, want %q", roundTripped.Warnings[0].Message, wantMsg)
	}
}

func TestWarning_JSONTags_AreLowerSnakeCase(t *testing.T) {
	w := Warning{Code: "x", Message: "y"}
	data, err := json.Marshal(w)
	if err != nil {
		t.Fatalf("Marshal failed: %v", err)
	}
	want := `{"code":"x","message":"y"}`
	if string(data) != want {
		t.Errorf("Marshal(Warning{Code:\"x\",Message:\"y\"}) = %s, want %s", data, want)
	}
}
