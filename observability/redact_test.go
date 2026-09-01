package observability

import (
	"encoding/json"
	"net/http"
	"strings"
	"testing"
)

func TestRedactHeadersDropsSensitiveValues(t *testing.T) {
	headers := http.Header{}
	headers.Set("Authorization", "Bearer super-secret-token")
	headers.Set("Cookie", "session=super-secret-cookie")
	headers.Set("Content-Type", "application/json")

	redacted := RedactHeaders(headers)

	if redacted["authorization"] != redactedValue {
		t.Fatalf("authorization = %q, want %q", redacted["authorization"], redactedValue)
	}
	if redacted["cookie"] != redactedValue {
		t.Fatalf("cookie = %q, want %q", redacted["cookie"], redactedValue)
	}
	if redacted["content-type"] != "application/json" {
		t.Fatalf("content-type = %q, want %q", redacted["content-type"], "application/json")
	}

	serialized, err := json.Marshal(redacted)
	if err != nil {
		t.Fatalf("marshal redacted headers: %v", err)
	}
	if strings.Contains(string(serialized), "super-secret-token") {
		t.Fatalf("serialized headers leaked token: %s", serialized)
	}
	if strings.Contains(string(serialized), "super-secret-cookie") {
		t.Fatalf("serialized headers leaked cookie: %s", serialized)
	}
}

func TestRedactJSONKeysRedactsNestedTokenFields(t *testing.T) {
	input := map[string]any{
		"access_token": "aaa",
		"nested": map[string]any{
			"refresh_token": "bbb",
			"ok":            123,
			"arr": []any{
				map[string]any{"id_token": "ccc"},
			},
		},
	}

	out := RedactJSONKeys(input)

	serialized, err := json.Marshal(out)
	if err != nil {
		t.Fatalf("marshal redacted json: %v", err)
	}
	s := string(serialized)
	if strings.Contains(s, `"aaa"`) {
		t.Fatalf("serialized json leaked access_token value: %s", s)
	}
	if strings.Contains(s, `"bbb"`) {
		t.Fatalf("serialized json leaked refresh_token value: %s", s)
	}
	if strings.Contains(s, `"ccc"`) {
		t.Fatalf("serialized json leaked id_token value: %s", s)
	}
	if !strings.Contains(s, redactedValue) {
		t.Fatalf("serialized json missing redacted marker: %s", s)
	}
}

func TestRedactJSONKeysCaseInsensitiveTopLevelToken(t *testing.T) {
	input := map[string]any{
		"Token": "secret",
		"other": "keep-me",
	}

	out := RedactJSONKeys(input).(map[string]any)

	if out["Token"] != redactedValue {
		t.Fatalf("Token = %v, want %q", out["Token"], redactedValue)
	}
	if out["other"] != "keep-me" {
		t.Fatalf("other = %v, want %q", out["other"], "keep-me")
	}
}

func TestRedactJSONKeysLeavesNonObjectValuesUntouched(t *testing.T) {
	input := []any{"a", 1, true, nil}

	out := RedactJSONKeys(input)

	outSlice, ok := out.([]any)
	if !ok {
		t.Fatalf("expected []any, got %T", out)
	}
	if len(outSlice) != 4 || outSlice[0] != "a" || outSlice[1] != 1 || outSlice[2] != true || outSlice[3] != nil {
		t.Fatalf("unexpected output: %#v", outSlice)
	}
}
