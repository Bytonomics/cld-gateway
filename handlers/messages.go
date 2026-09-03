// Package handlers holds thin Echo handlers, constructor-injected with the
// core/domain/services use-case interface they front. See FILEMAP.md
// "handlers/".
package handlers

import (
	"bytes"
	"io"
	"net/http"

	"github.com/labstack/echo/v4"

	"github.com/Bytonomics/cld-gateway/core/domain/dto"
	"github.com/Bytonomics/cld-gateway/core/domain/services"
	coresvc "github.com/Bytonomics/cld-gateway/core/impl/services"
	"github.com/Bytonomics/cld-gateway/middleware"
)

// claudeSessionIDHeader is the header Claude Code emits per turn to scope
// conversation-state branch selection, ported from
// claude_session_id_from_headers (lib.rs:3727-3735).
const claudeSessionIDHeader = "x-claude-code-session-id"

// MessagesHandler serves POST /v1/messages.
type MessagesHandler struct {
	svc services.MessageService
}

// NewMessagesHandler constructs a MessagesHandler.
func NewMessagesHandler(s services.MessageService) *MessagesHandler {
	return &MessagesHandler{svc: s}
}

// Post binds+validates the request (via the pedantigoecho binder installed
// as e.Binder in app/router.go) and calls MessageService.Handle. Exchange
// logging is no longer this handler's job - middleware.Capture, mounted on
// this route in app/routes_messages.go, wraps the whole call (unary and
// streaming both) and logs exactly once, uniformly with every other route
// (see middleware/capture.go's doc comment and ADR-0013 for why this is
// safe on the streaming path). Per-request metadata Capture can't know
// about generically (dto.MessagesResponse.ContextManagementMetadata) is
// handed to it via c.Set before returning.
//
// For MessageResult.Stream, this hands the channel to a
// core/impl/services.StreamWriter, called synchronously - StreamWriter is
// still the sole writer of SSE bytes to c.Response() for the duration of
// the exchange (unchanged invariant: about not racing two goroutines on one
// writer, not about middleware), and because the call is synchronous,
// Capture's post-next(c) logging still runs strictly after the last byte
// reaches the client.
func (h *MessagesHandler) Post(c echo.Context) error {
	rawBody, readErr := io.ReadAll(c.Request().Body)
	if readErr != nil {
		return readErr
	}
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

	result := h.svc.Handle(ctx, &req)
	if result.Err != nil {
		return result.Err
	}

	if result.ContextManagementMetadata != nil {
		c.Set("exchange_metadata", result.ContextManagementMetadata)
	}

	if result.Stream != nil {
		writer := coresvc.NewStreamWriter(c.Response(), 0)
		writer.Run(ctx, result.Stream)
		return nil
	}

	return c.JSON(http.StatusOK, result.Unary)
}
