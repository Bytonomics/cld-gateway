package handlers

import (
	"net/http"
	"time"

	"github.com/labstack/echo/v4"
)

// HealthHandler serves GET /health. Per FILEMAP.md/✱G11, no dedicated
// domain service exists for health - it summarizes process uptime and
// whether config.Load succeeded at startup.
type HealthHandler struct {
	startedAt    time.Time
	configLoaded bool
}

// NewHealthHandler constructs a HealthHandler. startedAt is the process (or
// server) start time; configLoaded reflects whether app.Initialize loaded
// gateway config successfully.
func NewHealthHandler(startedAt time.Time, configLoaded bool) *HealthHandler {
	return &HealthHandler{startedAt: startedAt, configLoaded: configLoaded}
}

// Get returns an uptime + config-loaded summary.
func (h *HealthHandler) Get(c echo.Context) error {
	return c.JSON(http.StatusOK, map[string]any{
		"status":         "ok",
		"uptime_seconds": int64(time.Since(h.startedAt).Seconds()),
		"config_loaded":  h.configLoaded,
	})
}
