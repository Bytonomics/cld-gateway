// stream_writer.go is the single SSE writer goroutine described in
// ARCHITECTURE_v2.md ("SSE + logging (settled)"): one goroutine owns the
// echo.Response for a streaming /v1/messages exchange, writes+flushes every
// event as it arrives, and hands the accumulated events to Option-C
// exchange logging only after the stream has finished.
package services

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"time"

	"github.com/labstack/echo/v4"

	"github.com/Bytonomics/cld-gateway/config"
	"github.com/Bytonomics/cld-gateway/core"
	"github.com/Bytonomics/cld-gateway/core/domain/dto"
	apperr "github.com/Bytonomics/cld-gateway/core/domain/errors"
	"github.com/Bytonomics/cld-gateway/observability"
)

// DefaultIdleEventTimeout is the ✱G4 idle-event timeout: if no event
// arrives on the input channel within this window, StreamWriter finalizes
// the stream instead of hanging forever.
const DefaultIdleEventTimeout = 60 * time.Second

// StreamLogEntry is the caller-captured half of the Option-C exchange log
// entry (request side, timing origin) that StreamWriter merges with the
// events it wrote before calling log.Append at stream close.
type StreamLogEntry struct {
	RequestID           core.RequestID
	StartedAtUnixMs     int64
	ModelResolution     *config.ModelResolution
	ContextManagementMd map[string]any
	Request             observability.HTTPRequestRecord
}

// StreamWriter is the sole owner of resp for the lifetime of one streaming
// exchange. No middleware may wrap resp on the streaming path (enforced by
// wiring in a later wave); StreamWriter itself assumes nothing about
// wrapping - it writes and flushes resp directly.
type StreamWriter struct {
	resp        *echo.Response
	idleTimeout time.Duration
	headersSent bool
}

// NewStreamWriter builds a StreamWriter over resp. idleTimeout <= 0 falls
// back to DefaultIdleEventTimeout.
func NewStreamWriter(resp *echo.Response, idleTimeout time.Duration) *StreamWriter {
	if idleTimeout <= 0 {
		idleTimeout = DefaultIdleEventTimeout
	}
	return &StreamWriter{resp: resp, idleTimeout: idleTimeout}
}

// WriteEvent writes one Anthropic SSE frame ("event: ..\ndata: ..\n\n") and
// flushes it immediately. Headers are written before the first call.
// Callers must only invoke this from the single goroutine that owns resp.
func (w *StreamWriter) WriteEvent(ev dto.SSEEvent) error {
	w.ensureHeaders()
	if _, err := fmt.Fprintf(w.resp, "event: %s\ndata: %s\n\n", ev.Event, ev.Data); err != nil {
		return err
	}
	w.resp.Flush()
	return nil
}

func (w *StreamWriter) ensureHeaders() {
	if w.headersSent {
		return
	}
	w.headersSent = true
	header := w.resp.Header()
	header.Set(echo.HeaderContentType, "text/event-stream")
	header.Set("Cache-Control", "no-cache")
	header.Set("Connection", "keep-alive")
	w.resp.WriteHeader(http.StatusOK)
}

// Run drains in until MessageService closes it (services.MessageResult's
// documented channel contract) or ctx is done, writing every event via
// WriteEvent. Run is meant to be called synchronously by the HTTP handler
// (it blocks until the stream ends) so that no byte is ever written to
// resp after the handler has returned.
//
// If no event arrives within the idle timeout (✱G4), Run writes a
// terminal error SSE event and stops - since message_service.go remains
// the only permitted closer of in, Run keeps draining (and discarding) in
// on a background goroutine afterward so the producer is never left
// blocked on an unbuffered send.
//
// At close, Run hands the accumulated events to log.Append (Option C):
// this happens strictly after the last byte reaches the client, so
// logging never delays streaming.
func (w *StreamWriter) Run(ctx context.Context, in <-chan dto.SSEEvent, base StreamLogEntry, log observability.ExchangeLog) {
	var accumulated []dto.SSEEvent

	timer := time.NewTimer(w.idleTimeout)
	defer timer.Stop()

	finalize := func(terminalReason string) {
		if terminalReason != "" {
			ev := finalizeErrorEvent(terminalReason)
			accumulated = append(accumulated, ev)
			_ = w.WriteEvent(ev)
		}
		go drainSSEChannel(in)
		w.logAccumulated(base, accumulated, log)
	}

	for {
		if !timer.Stop() {
			select {
			case <-timer.C:
			default:
			}
		}
		timer.Reset(w.idleTimeout)

		select {
		case ev, ok := <-in:
			if !ok {
				w.logAccumulated(base, accumulated, log)
				return
			}
			accumulated = append(accumulated, ev)
			if err := w.WriteEvent(ev); err != nil {
				// Client is gone; stop writing but keep draining in so
				// message_service.go's close(in) never blocks.
				go drainSSEChannel(in)
				w.logAccumulated(base, accumulated, log)
				return
			}
		case <-ctx.Done():
			finalize("")
			return
		case <-timer.C:
			finalize("idle_timeout")
			return
		}
	}
}

func drainSSEChannel(in <-chan dto.SSEEvent) {
	for range in {
	}
}

func finalizeErrorEvent(reason string) dto.SSEEvent {
	// Reuses errors.Classify only for message-branding consistency (the
	// "[CLD-Gateway] " prefix, same as every other error surface) - an
	// idle-timeout or client-abort termination is expected, self-inflicted
	// behavior, never a gateway defect, so SuggestIssue/Instruction from
	// Classify are deliberately discarded here, not surfaced.
	gwErr := apperr.Classify(apperr.New(apperr.CodeAPI, "stream terminated: "+reason, 0))
	payload, err := json.Marshal(map[string]any{
		"type": "error",
		"error": map[string]any{
			"type":    string(gwErr.Code),
			"message": gwErr.Message,
		},
	})
	if err != nil {
		// Fallback if marshaling fails (should be extremely rare for simple maps)
		payload = []byte(`{"type":"error","error":{"type":"api_error","message":"stream terminated"}}`)
	}
	return dto.SSEEvent{Event: "error", Data: payload}
}

func (w *StreamWriter) logAccumulated(base StreamLogEntry, events []dto.SSEEvent, log observability.ExchangeLog) {
	if log == nil {
		return
	}
	var buf bytes.Buffer
	for _, ev := range events {
		buf.WriteString("event: ")
		buf.WriteString(ev.Event)
		buf.WriteString("\ndata: ")
		buf.Write(ev.Data)
		buf.WriteString("\n\n")
	}

	entry := observability.Entry{
		RequestID:       base.RequestID,
		StartedAtUnixMs: base.StartedAtUnixMs,
		DurationMs:      time.Now().UnixMilli() - base.StartedAtUnixMs,
		ModelResolution: base.ModelResolution,
		Metadata:        base.ContextManagementMd,
		Request:         base.Request,
		Response: observability.HTTPResponseRecord{
			Status: w.resp.Status,
			Body:   observability.CaptureBody("text/event-stream", buf.Bytes()),
		},
	}
	_ = log.Append(entry)
}
