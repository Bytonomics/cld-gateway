package observability

import (
	"encoding/json"
	"unicode/utf8"

	"github.com/Bytonomics/cld-gateway/config"
	"github.com/Bytonomics/cld-gateway/core"
)

// BodyLimitBytes caps how many bytes of a request/response body are
// captured for logging.
const BodyLimitBytes = 5 * 1024 * 1024

// BodyKind identifies the shape a CapturedBody's payload was captured in.
type BodyKind string

const (
	BodyEmpty  BodyKind = "empty"
	BodyJSON   BodyKind = "json"
	BodyText   BodyKind = "text"
	BodyBinary BodyKind = "binary"
)

// CapturedBody holds a size- and shape-bounded snapshot of an HTTP body.
type CapturedBody struct {
	ContentType   string
	BytesCaptured int
	Truncated     bool
	Kind          BodyKind
	JSON          any
	Text          string
	Note          string
}

// HTTPRequestRecord is the captured shape of an inbound HTTP request.
type HTTPRequestRecord struct {
	Method  string
	URI     string
	Headers map[string]string
	Body    CapturedBody
}

// HTTPResponseRecord is the captured shape of an outbound HTTP response.
type HTTPResponseRecord struct {
	Status  int
	Headers map[string]string
	Body    CapturedBody
}

// Entry is one exchange log record: a single HTTP request/response pair
// plus timing and (optional) backend model-resolution info.
type Entry struct {
	RequestID       core.RequestID
	StartedAtUnixMs int64
	DurationMs      int64
	ModelResolution *config.ModelResolution
	Request         HTTPRequestRecord
	Response        HTTPResponseRecord
}

// ExchangeLog persists exchange entries.
type ExchangeLog interface {
	Append(entry Entry) error
}

// CaptureBody applies the 5MB body-capture limit and shape detection
// (json/text/binary/empty) to a raw body, redacting JSON keys via
// RedactJSONKeys. Mirrors build_captured_body in
// crates/gateway-observability/src/middleware.rs.
func CaptureBody(contentType string, raw []byte) CapturedBody {
	data := raw
	truncated := false
	if len(data) > BodyLimitBytes {
		data = data[:BodyLimitBytes]
		truncated = true
	}
	bytesCaptured := len(data)

	if truncated {
		return CapturedBody{
			ContentType:   contentType,
			BytesCaptured: bytesCaptured,
			Truncated:     true,
			Kind:          BodyBinary,
			Note:          "body capture skipped: exceeded 5242880 bytes limit",
		}
	}
	if bytesCaptured == 0 {
		return CapturedBody{
			ContentType:   contentType,
			BytesCaptured: 0,
			Truncated:     false,
			Kind:          BodyEmpty,
		}
	}
	if utf8.Valid(data) {
		var v any
		if err := json.Unmarshal(data, &v); err == nil {
			return CapturedBody{
				ContentType:   contentType,
				BytesCaptured: bytesCaptured,
				Truncated:     false,
				Kind:          BodyJSON,
				JSON:          RedactJSONKeys(v),
			}
		}
		return CapturedBody{
			ContentType:   contentType,
			BytesCaptured: bytesCaptured,
			Truncated:     false,
			Kind:          BodyText,
			Text:          string(data),
		}
	}
	return CapturedBody{
		ContentType:   contentType,
		BytesCaptured: bytesCaptured,
		Truncated:     false,
		Kind:          BodyBinary,
		Note:          "non-utf8 body captured as metadata only",
	}
}
