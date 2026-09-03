// stream_writer.go is the single SSE writer described in ADR-0013 (which
// supersedes ADR-0004's "Option C" logging): one goroutine owns the
// echo.Response for a streaming /v1/messages exchange, writes+flushes every
// event as it arrives; exchange logging is middleware.Capture's job now,
// not this file's.
package services

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"time"

	"github.com/labstack/echo/v4"

	"github.com/Bytonomics/cld-gateway/core/domain/dto"
	apperr "github.com/Bytonomics/cld-gateway/core/domain/errors"
)

// DefaultIdleEventTimeout is the ✱G4 idle-event timeout: if no event
// arrives on the input channel within this window, StreamWriter finalizes
// the stream instead of hanging forever.
const DefaultIdleEventTimeout = 60 * time.Second

// StreamWriter is the sole owner of resp for the lifetime of one streaming
// exchange - exactly one goroutine ever writes SSE bytes to it, so there is
// no write race to guard against. Middleware MAY wrap resp's underlying
// writer (middleware.Capture does, as of ADR-0013) as long as the wrapper
// implements Unwrap() http.ResponseWriter so Flush()/WriteHeader() still
// reach the real writer; StreamWriter itself writes and flushes resp
// exactly as before, unaware of and unaffected by any such wrapping.
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
// resp after the handler has returned - this is also what makes
// middleware.Capture's post-next(c) exchange logging correct for streaming
// responses: it wraps the whole synchronous handler call, so its logging
// still runs strictly after the last byte reaches the client (see
// middleware/capture.go's doc comment and ADR-0013). Run no longer logs
// the exchange itself (ADR-0004's Option C is superseded by ADR-0013) -
// Capture is the single place that does it now, for every route.
//
// If no event arrives within the idle timeout (✱G4), Run writes a
// terminal error SSE event and stops - since message_service.go remains
// the only permitted closer of in, Run keeps draining (and discarding) in
// on a background goroutine afterward so the producer is never left
// blocked on an unbuffered send.
func (w *StreamWriter) Run(ctx context.Context, in <-chan dto.SSEEvent) {
	timer := time.NewTimer(w.idleTimeout)
	defer timer.Stop()

	finalize := func(terminalReason string) {
		if terminalReason != "" {
			_ = w.WriteEvent(finalizeErrorEvent(terminalReason))
		}
		go drainSSEChannel(in)
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
				return
			}
			if err := w.WriteEvent(ev); err != nil {
				// Client is gone; stop writing but keep draining in so
				// message_service.go's close(in) never blocks.
				go drainSSEChannel(in)
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
