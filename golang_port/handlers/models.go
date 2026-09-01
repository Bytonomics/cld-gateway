package handlers

import (
	"net/http"

	"github.com/labstack/echo/v4"

	"github.com/Bytonomics/cld-gateway/core/domain/services"
)

// ModelsHandler serves GET /v1/models.
type ModelsHandler struct {
	svc services.ModelsService
}

// NewModelsHandler constructs a ModelsHandler.
func NewModelsHandler(s services.ModelsService) *ModelsHandler {
	return &ModelsHandler{svc: s}
}

// List returns the model catalog.
func (h *ModelsHandler) List(c echo.Context) error {
	list, err := h.svc.List(c.Request().Context())
	if err != nil {
		return err
	}
	return c.JSON(http.StatusOK, list)
}
