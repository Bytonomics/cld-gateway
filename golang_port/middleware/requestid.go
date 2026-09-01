// Package middleware holds Echo middleware shared across handlers/routes:
// request-id propagation, panic recovery (AppError->Anthropic shape), and
// unary-only exchange capture. See ARCHITECTURE_v2.md ("SSE + logging").
package middleware

import (
	"github.com/labstack/echo/v4"

	"github.com/Bytonomics/cld-gateway/core"
	coresvc "github.com/Bytonomics/cld-gateway/core/impl/services"
)

// ResponseHeader is the response header carrying the per-request id, ported
// from gateway-observability's "Adds x-proxy-request-id header to responses
// for correlation" (crates/gateway-observability/src/middleware.rs).
const ResponseHeader = "x-proxy-request-id"

// contextKey is the echo.Context key RequestID stashes the id under, for
// handlers that only have an echo.Context (not a context.Context) at hand.
const contextKey = "cld_gateway_request_id"

// RequestID mints a core.RequestID for every request, stores it on both the
// echo.Context (c.Get(contextKey)) and the request's context.Context (via
// core/impl/services.WithRequestID, so downstream MessageService.Handle
// calls see it through RequestIDFromContext without handlers re-threading
// it), and stamps the response header for client-side correlation. Headers
// are set before next(c) runs so the id is present even if a later
// handler/middleware panics.
func RequestID() echo.MiddlewareFunc {
	return func(next echo.HandlerFunc) echo.HandlerFunc {
		return func(c echo.Context) error {
			id := core.NewRequestID()
			c.Set(contextKey, id)
			c.Response().Header().Set(ResponseHeader, id.String())

			ctx := coresvc.WithRequestID(c.Request().Context(), id)
			c.SetRequest(c.Request().WithContext(ctx))

			return next(c)
		}
	}
}

// RequestIDFromEcho reads back the id RequestID() stashed on c, minting a
// fresh one (without storing it) if the middleware was never run - mirrors
// core/impl/services.RequestIDFromContext's "never operate without a lease
// discriminant" fallback.
func RequestIDFromEcho(c echo.Context) core.RequestID {
	if v, ok := c.Get(contextKey).(core.RequestID); ok && v != "" {
		return v
	}
	return coresvc.RequestIDFromContext(c.Request().Context())
}
