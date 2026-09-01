package app

import (
	"time"

	"github.com/labstack/echo/v4"

	"github.com/Bytonomics/cld-gateway/handlers"
	"github.com/Bytonomics/cld-gateway/middleware"
)

// MountMetaAPI mounts GET /health, GET /v1/models, GET /auth/status, and
// POST /auth/refresh: the always-unary routes, so each is safe to wrap with
// middleware.Capture (see middleware/capture.go's package doc for why this
// must never be mounted on POST /v1/messages instead).
func MountMetaAPI(e *echo.Echo, p *Providers) {
	capture := middleware.Capture(p.ExchangeLog)

	// startedAt approximates process start as "when this router was built";
	// NewEcho runs immediately after app.Initialize succeeds, so
	// configLoaded is always true by the time a request can reach /health.
	health := handlers.NewHealthHandler(time.Now(), true)
	e.GET("/health", health.Get, capture)

	models := handlers.NewModelsHandler(p.ModelsService)
	e.GET("/v1/models", models.List, capture)

	authHandler := handlers.NewAuthHandler(p.AuthStatusService)
	e.GET("/auth/status", authHandler.Status, capture)
	e.POST("/auth/refresh", authHandler.Refresh, capture)
}
