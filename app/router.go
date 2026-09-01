package app

import (
	pedantigoecho "github.com/SmrutAI/pedantigo/plugins/web/pedantigoecho/v2"
	"github.com/labstack/echo/v4"

	"github.com/Bytonomics/cld-gateway/middleware"
)

// NewEcho wires the Echo router for one gatewayd process: the
// pedantigo-backed binder (POST/PUT/PATCH bodies validated via the
// `validate` tag; GET/DELETE/HEAD fall back to path/query params), the
// central AppError->Anthropic HTTPErrorHandler, panic recovery, and the
// per-request id middleware, then mounts the meta and messages route
// groups. Per ARCHITECTURE_v2.md ("SSE + logging") and
// middleware/capture.go's package doc, middleware.Capture is mounted ONLY
// inside MountMetaAPI's always-unary routes - it must never wrap
// POST /v1/messages, whose handler hands the response writer to the single
// SSE-writer goroutine.
func NewEcho(p *Providers) *echo.Echo {
	e := echo.New()
	e.HideBanner = true
	e.HidePort = true

	e.Binder = pedantigoecho.NewBinder()
	e.HTTPErrorHandler = middleware.ErrorHandler

	e.Use(middleware.RequestID())
	e.Use(middleware.Recover())

	MountMetaAPI(e, p)
	MountMessagesAPI(e, p)

	return e
}

// RunServer starts e listening on addr. ✱G3 (graceful shutdown via
// signal.NotifyContext + Shutdown drain) is an open, not-yet-owner-approved
// gap - this stays a plain e.Start(addr), matching cmd/cld-gateway/main.go's
// current non-graceful run_serve (ported as-is from Rust's non-graceful
// axum::serve(...).await, per main.go's own runServe doc comment).
func RunServer(e *echo.Echo, addr string) error {
	return e.Start(addr)
}
