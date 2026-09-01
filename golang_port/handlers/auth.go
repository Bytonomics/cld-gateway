package handlers

import (
	"net/http"

	"github.com/labstack/echo/v4"

	"github.com/Bytonomics/cld-gateway/core/domain/services"
)

// AuthHandler serves GET /auth/status and POST /auth/refresh.
type AuthHandler struct {
	svc services.AuthStatusService
}

// NewAuthHandler constructs an AuthHandler.
func NewAuthHandler(s services.AuthStatusService) *AuthHandler {
	return &AuthHandler{svc: s}
}

// Status returns the current auth status.
func (h *AuthHandler) Status(c echo.Context) error {
	status, err := h.svc.Status(c.Request().Context())
	if err != nil {
		return err
	}
	return c.JSON(http.StatusOK, status)
}

// Refresh forces a token refresh and returns the resulting auth status.
func (h *AuthHandler) Refresh(c echo.Context) error {
	status, err := h.svc.Refresh(c.Request().Context())
	if err != nil {
		return err
	}
	return c.JSON(http.StatusOK, status)
}
