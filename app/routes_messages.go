package app

import (
	"github.com/labstack/echo/v4"

	"github.com/Bytonomics/cld-gateway/handlers"
	"github.com/Bytonomics/cld-gateway/middleware"
)

// MountMessagesAPI mounts POST /v1/messages and POST /v1/messages/count_tokens.
// POST /v1/messages carries NO middleware.Capture: it can stream, and
// performs its own unary-path logging inline, handing streaming responses
// to the single SSE-writer goroutine instead (handlers/messages.go's doc
// comment; middleware/capture.go's package doc explains why wrapping the
// response writer here would be unsafe). count_tokens is always-unary, so
// it is safe to wrap with middleware.Capture like the meta routes.
func MountMessagesAPI(e *echo.Echo, p *Providers) {
	messages := handlers.NewMessagesHandler(p.MessageService, p.ExchangeLog)
	e.POST("/v1/messages", messages.Post)

	countTokens := handlers.NewCountTokensHandler(p.CountTokensService)
	e.POST("/v1/messages/count_tokens", countTokens.Post, middleware.Capture(p.ExchangeLog))
}
