package handlers

import (
	"net/http"

	"github.com/labstack/echo/v4"

	"github.com/Bytonomics/cld-gateway/core/domain/dto"
	"github.com/Bytonomics/cld-gateway/core/domain/services"
)

// CountTokensHandler serves POST /v1/messages/count_tokens.
type CountTokensHandler struct {
	svc services.CountTokensService
}

// NewCountTokensHandler constructs a CountTokensHandler.
func NewCountTokensHandler(s services.CountTokensService) *CountTokensHandler {
	return &CountTokensHandler{svc: s}
}

// Post binds the same request shape as POST /v1/messages and returns the
// estimated input token count, ported from v1_messages_count_tokens
// (lib.rs:891-912).
func (h *CountTokensHandler) Post(c echo.Context) error {
	var req dto.MessagesRequest
	if err := c.Bind(&req); err != nil {
		return err
	}

	tokens := h.svc.Estimate(c.Request().Context(), &req)
	return c.JSON(http.StatusOK, dto.CountTokensResponse{InputTokens: tokens})
}
