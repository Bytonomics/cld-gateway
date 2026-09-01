package codex

import (
	"bufio"
	"context"
	"encoding/json"
	"io"
	"strings"
	"time"

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
// backend.Event's contract, Type is parsed from the SSE "event:" line
// (defaulting to "message" per the WHATWG EventSource spec when absent),
// matching the wire format where Codex Responses backend sends event types
// like "response.output_text.delta" as SSE event: names, not as JSON payload
// fields.
//
// idleTimeout specifies the maximum time to wait between receiving SSE lines.
// If no line arrives within this window, the decoder closes the body,
// emits an error event, and stops — preventing goroutine leaks when the
// upstream stalls. idleTimeout <= 0 is treated as infinite (no timeout).
//
// The returned channel is closed when body is exhausted, ctx is cancelled,
// a scan error occurs, or the idle timeout fires; a scan error or idle
// timeout is surfaced as one final Event{Type:"error"} frame before the
// channel closes, mirroring the WebSocket-stage error mapping
// response_to_event_stream applies to decode failures (client.rs:283-289).
// DecodeEventStream takes ownership of body and closes it.
func DecodeEventStream(ctx context.Context, body io.ReadCloser, idleTimeout time.Duration) <-chan backend.Event {
	out := make(chan backend.Event)

	go func() {
		defer close(out)
		defer func() { _ = body.Close() }()

		scanner := bufio.NewScanner(body)
		scanner.Buffer(make([]byte, 0, sseScanBufferBytes), sseMaxLineBytes)

		var dataLines []string
		var eventName string
		emit := func() bool {
			if len(dataLines) == 0 {
				return true
			}
			data := strings.Join(dataLines, "\n")
			dataLines = dataLines[:0]
			eventName = ""

			// Use eventName from SSE event: line; default to "message" per WHATWG spec.
			eventType := eventName
			if eventType == "" {
				eventType = "message"
			}

			select {
			case out <- backend.Event{Type: eventType, Data: json.RawMessage(data)}:
				return true
			case <-ctx.Done():
				return false
			}
		}

		// emitError marshals and sends an error event, consuming up to ctx.Done.
		emitError := func(msg string) {
			errData, err := json.Marshal(map[string]string{
				"type":    "error",
				"message": msg,
			})
			if err != nil {
				// json.Marshal should never fail for a simple string map.
				// If it does, use a fallback without the message field.
				errData = []byte(`{"type":"error"}`)
			}
			select {
			case out <- backend.Event{Type: "error", Data: errData}:
			case <-ctx.Done():
			}
		}

		// Inner goroutine races scanner.Scan() against the idle timeout.
		// scanResults carries (line, ok, err) tuples from the scanner.
		type scanResult struct {
			line string
			ok   bool
			err  error
		}
		scanResults := make(chan scanResult)

		go func() {
			for scanner.Scan() {
				select {
				case scanResults <- scanResult{line: scanner.Text(), ok: true, err: nil}:
				case <-ctx.Done():
					return
				}
			}
			scanResults <- scanResult{line: "", ok: false, err: scanner.Err()}
			close(scanResults)
		}()

		// If idleTimeout <= 0, treat as infinite.
		var timer *time.Timer
		var timerChan <-chan time.Time
		if idleTimeout > 0 {
			timer = time.NewTimer(idleTimeout)
			defer timer.Stop()
			timerChan = timer.C
		}

		for {
			// Reset or create timer for this iteration.
			if timer != nil {
				if !timer.Stop() {
					select {
					case <-timer.C:
					default:
					}
				}
				timer.Reset(idleTimeout)
			}

			select {
			case result, ok := <-scanResults:
				if !ok {
					// Scanner closed (end of stream or context done).
					if !emit() {
						return
					}
					if result.err != nil {
						emitError(result.err.Error())
					}
					return
				}

				if !result.ok {
					// Scan failed.
					if !emit() {
						return
					}
					if result.err != nil {
						emitError(result.err.Error())
					}
					return
				}

				// Scan succeeded; process the line.
				line := result.line
				switch {
				case line == "":
					if !emit() {
						return
					}
				case strings.HasPrefix(line, ":"):
					// SSE comment/keep-alive line; ignored.
				case strings.HasPrefix(line, "event:"):
					// Capture the SSE event: line name (trimming leading space per SSE spec).
					eventName = strings.TrimPrefix(strings.TrimPrefix(line, "event:"), " ")
				case strings.HasPrefix(line, "data:"):
					dataLines = append(dataLines, strings.TrimPrefix(strings.TrimPrefix(line, "data:"), " "))
				default:
					// Other SSE fields (id:, retry:) are ignored.
				}

			case <-ctx.Done():
				if !emit() {
					return
				}
				return

			case <-timerChan:
				// Idle timeout fired: close body to unblock inner goroutine's
				// Read, emit error, and stop.
				_ = body.Close()
				emitError("idle_read_timeout")
				return
			}
		}
	}()

	return out
}
