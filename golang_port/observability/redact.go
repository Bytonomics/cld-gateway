// Package observability implements exchange logging and redaction for the
// gateway's HTTP request/response capture pipeline.
package observability

import (
	"net/http"
	"strings"
)

const redactedValue = "[REDACTED]"

var redactedHeaderNames = map[string]bool{
	"authorization":       true,
	"cookie":              true,
	"set-cookie":          true,
	"proxy-authorization": true,
}

var redactedJSONKeys = map[string]bool{
	"access_token":  true,
	"refresh_token": true,
	"id_token":      true,
	"token":         true,
}

// RedactHeaders returns a lowercase-keyed copy of headers with sensitive
// header values replaced by redactedValue. Multi-valued headers are
// flattened with ", " as in http.Header.Get semantics.
func RedactHeaders(headers http.Header) map[string]string {
	out := make(map[string]string, len(headers))
	for name, values := range headers {
		nameLower := strings.ToLower(name)
		if redactedHeaderNames[nameLower] {
			out[nameLower] = redactedValue
			continue
		}
		out[nameLower] = strings.Join(values, ", ")
	}
	return out
}

// RedactJSONKeys recursively walks a decoded JSON value (as produced by
// encoding/json.Unmarshal into any) and replaces the values of sensitive
// keys with redactedValue, at any nesting depth through objects and arrays.
func RedactJSONKeys(value any) any {
	switch v := value.(type) {
	case map[string]any:
		out := make(map[string]any, len(v))
		for k, val := range v {
			if redactedJSONKeys[strings.ToLower(k)] {
				out[k] = redactedValue
			} else {
				out[k] = RedactJSONKeys(val)
			}
		}
		return out
	case []any:
		out := make([]any, len(v))
		for i, item := range v {
			out[i] = RedactJSONKeys(item)
		}
		return out
	default:
		return v
	}
}
