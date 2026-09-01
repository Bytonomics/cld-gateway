package codex

import (
	"bufio"
	"context"
	"encoding/json"
	"io"
	"strings"

	"github.com/Bytonomics/cld-gateway/core/domain/port/backend"
)

// sseScanBufferBytes / sseMaxLineBytes bound bufio.Scanner's per-line buffer
// so a single oversized SSE line cannot grow it unbounded.
const (
	sseScanBufferBytes = 64 * 1024
	sseMaxLineBytes    = 8 * 1024 * 1024
)

// DecodeEventStream decodes an EventSource-formatted (text/event-stream)
// response body into a channel of backend.Event, mirroring
// eventsource_stream::Eventsource as used by
// CodexBackendClient::response_to_event_stream (client.rs:276-291). Per
// backend.Event's contract ("Type from parsed \"type\"; Data = full raw
// JSON", types.go), Type is parsed from the "type" field inside each frame's
// JSON data — the Codex Responses payload carries its own event type there —
// not from the SSE "event:" line, which this decoder otherwise ignores.
//
// The returned channel is closed when body is exhausted, ctx is cancelled,
// or a scan error occurs; a scan error is surfaced as one final
// Event{Type:"error"} frame before the channel closes, mirroring the
// WebSocket-stage error mapping response_to_event_stream applies to decode
// failures (client.rs:283-289). DecodeEventStream takes ownership of body
// and closes it.
func DecodeEventStream(ctx context.Context, body io.ReadCloser) <-chan backend.Event {
	out := make(chan backend.Event)

	go func() {
		defer close(out)
		defer func() { _ = body.Close() }()

		scanner := bufio.NewScanner(body)
		scanner.Buffer(make([]byte, 0, sseScanBufferBytes), sseMaxLineBytes)

		var dataLines []string
		emit := func() bool {
			if len(dataLines) == 0 {
				return true
			}
			data := strings.Join(dataLines, "\n")
			dataLines = dataLines[:0]
			if data == "[DONE]" {
				return true
			}

			var probe struct {
				Type string `json:"type"`
			}
			_ = json.Unmarshal([]byte(data), &probe)

			select {
			case out <- backend.Event{Type: probe.Type, Data: json.RawMessage(data)}:
				return true
			case <-ctx.Done():
				return false
			}
		}

		for scanner.Scan() {
			line := scanner.Text()
			switch {
			case line == "":
				if !emit() {
					return
				}
			case strings.HasPrefix(line, ":"):
				// SSE comment/keep-alive line; ignored.
			case strings.HasPrefix(line, "data:"):
				dataLines = append(dataLines, strings.TrimPrefix(strings.TrimPrefix(line, "data:"), " "))
			default:
				// Other SSE fields (event:, id:, retry:) are ignored: Type
				// comes from the JSON payload per backend.Event's contract,
				// not the SSE event name.
			}
		}

		if !emit() {
			return
		}

		if err := scanner.Err(); err != nil {
			errData, _ := json.Marshal(map[string]string{
				"type":    "error",
				"message": err.Error(),
			})
			select {
			case out <- backend.Event{Type: "error", Data: errData}:
			case <-ctx.Done():
			}
		}
	}()

	return out
}
