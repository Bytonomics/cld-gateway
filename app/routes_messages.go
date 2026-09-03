package app

import (
	"github.com/labstack/echo/v4"

	"github.com/Bytonomics/cld-gateway/handlers"
	"github.com/Bytonomics/cld-gateway/middleware"
)

// MountMessagesAPI mounts POST /v1/messages and POST /v1/messages/count_tokens.
// Both carry middleware.Capture, including /v1/messages's streaming case -
// see middleware/capture.go's doc comment and ADR-0013 for why this is now
// safe (captureWriter implements Unwrap() http.ResponseWriter, which Go
// 1.20+'s http.ResponseController follows to find the real http.Flusher
// through the wrapper). handlers/messages.go no longer performs its own
// exchange logging; Capture is the single place that does it for every
// route, streaming included.
func MountMessagesAPI(e *echo.Echo, p *Providers) {
	messages := handlers.NewMessagesHandler(p.MessageService)
	e.POST("/v1/messages", messages.Post, middleware.Capture(p.ExchangeLog))

	countTokens := handlers.NewCountTokensHandler(p.CountTokensService)
	e.POST("/v1/messages/count_tokens", countTokens.Post, middleware.Capture(p.ExchangeLog))
}
