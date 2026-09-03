// Package handlers holds thin Echo handlers, constructor-injected with the
// core/domain/services use-case interface they front. See FILEMAP.md
// "handlers/".
package handlers

import (
	"bytes"
	"encoding/json"
	"io"
	"net/http"
	"time"

	"github.com/labstack/echo/v4"

	"github.com/Bytonomics/cld-gateway/core"
	"github.com/Bytonomics/cld-gateway/core/domain/dto"
	"github.com/Bytonomics/cld-gateway/core/domain/services"
	coresvc "github.com/Bytonomics/cld-gateway/core/impl/services"
	"github.com/Bytonomics/cld-gateway/middleware"
	"github.com/Bytonomics/cld-gateway/observability"
)

// claudeSessionIDHeader is the header Claude Code emits per turn to scope
// conversation-state branch selection, ported from
// claude_session_id_from_headers (lib.rs:3727-3735).
const claudeSessionIDHeader = "x-claude-code-session-id"

// MessagesHandler serves POST /v1/messages.
type MessagesHandler struct {
	svc services.MessageService
	log observability.ExchangeLog
}

// NewMessagesHandler constructs a MessagesHandler.
func NewMessagesHandler(s services.MessageService, l observability.ExchangeLog) *MessagesHandler {
	return &MessagesHandler{svc: s, log: l}
}

// Post binds+validates the request (via the pedantigoecho binder installed
// as e.Binder in app/router.go), calls MessageService.Handle, and:
//   - for MessageResult.Unary, writes the JSON response directly and logs
//     the exchange itself (see the package doc on middleware/capture.go for
//     why this route cannot rely on that middleware);
//   - for MessageResult.Stream, hands the channel to a
//     core/impl/services.StreamWriter and does NOT wrap c.Response() with
//     any further middleware - StreamWriter is the sole writer for the rest
//     of this request (ARCHITECTURE_v2.md, "SSE + logging").
func (h *MessagesHandler) Post(c echo.Context) error {
	rawBody, _ := io.ReadAll(c.Request().Body)
	_ = c.Request().Body.Close()
	c.Request().Body = io.NopCloser(bytes.NewReader(rawBody))

	var req dto.MessagesRequest
	if err := c.Bind(&req); err != nil {
		return err
	}

	reqID := middleware.RequestIDFromEcho(c)
	ctx := coresvc.WithRequestID(c.Request().Context(), reqID)
	if sessionID := c.Request().Header.Get(claudeSessionIDHeader); sessionID != "" {
		ctx = coresvc.WithClaudeSessionID(ctx, sessionID)
	}

	started := time.Now()
	result := h.svc.Handle(ctx, &req)
	if result.Err != nil {
		h.logError(c, reqID, started, rawBody, result.Err)
		return result.Err
	}

	if result.Stream != nil {
		writer := coresvc.NewStreamWriter(c.Response(), 0)
		writer.Run(ctx, result.Stream, coresvc.StreamLogEntry{
			RequestID:           reqID,
			StartedAtUnixMs:     started.UnixMilli(),
			ContextManagementMd: result.ContextManagementMetadata,
			Request:             h.requestRecord(c, rawBody),
		}, h.log)
		return nil
	}

	if err := c.JSON(http.StatusOK, result.Unary); err != nil {
		return err
	}
	h.logUnary(c, reqID, started, rawBody, result.Unary, result.ContextManagementMetadata)
	return nil
}

func (h *MessagesHandler) requestRecord(c echo.Context, rawBody []byte) observability.HTTPRequestRecord {
	return observability.HTTPRequestRecord{
		Method:  c.Request().Method,
		URI:     c.Request().RequestURI,
		Headers: observability.RedactHeaders(c.Request().Header),
		Body:    observability.CaptureBody(c.Request().Header.Get(echo.HeaderContentType), rawBody),
	}
}

// logError logs the exchange for a request that failed before any response
// was written (result.Err != nil in Post, above), mirroring the always-log
// behavior of the Rust port's capture_http_exchange middleware
// (gateway-observability/src/middleware.rs:42-140), which wraps every
// response - success or error - via `next.run(req).await`. Go's handler
// short-circuits on error instead of building a Response value the way Rust
// did, so without this call the exchange log would never see errored
// requests at all. Reuses middleware.ClassifyForResponse - the same
// function middleware.ErrorHandler and middleware.Capture use - so the
// logged status/body always match what the client actually received, and
// so /v1/messages gets identical error classification (origin, branding,
// bug-report guidance) to every other route instead of a separately
// duplicated derivation.
func (h *MessagesHandler) logError(c echo.Context, reqID core.RequestID, started time.Time, rawBody []byte, handleErr error) {
	if h.log == nil {
		return
	}
	status, payload := middleware.ClassifyForResponse(handleErr)
	respBody, err := json.Marshal(payload)
	if err != nil {
		respBody = nil
	}
	entry := observability.Entry{
		RequestID:       reqID,
		StartedAtUnixMs: started.UnixMilli(),
		DurationMs:      time.Since(started).Milliseconds(),
		Request:         h.requestRecord(c, rawBody),
		Response: observability.HTTPResponseRecord{
			Status:  status,
			Headers: observability.RedactHeaders(c.Response().Header()),
			Body:    observability.CaptureBody("application/json", respBody),
		},
	}
	_ = h.log.Append(entry)
}

func (h *MessagesHandler) logUnary(c echo.Context, reqID core.RequestID, started time.Time, rawBody []byte, resp *dto.MessagesResponse, metadata map[string]any) {
	if h.log == nil {
		return
	}
	respBody, err := json.Marshal(resp)
	if err != nil {
		respBody = nil
	}
	entry := observability.Entry{
		RequestID:       reqID,
		StartedAtUnixMs: started.UnixMilli(),
		DurationMs:      time.Since(started).Milliseconds(),
		Metadata:        metadata,
		Request:         h.requestRecord(c, rawBody),
		Response: observability.HTTPResponseRecord{
			Status:  c.Response().Status,
			Headers: observability.RedactHeaders(c.Response().Header()),
			Body:    observability.CaptureBody("application/json", respBody),
		},
	}
	_ = h.log.Append(entry)
}
