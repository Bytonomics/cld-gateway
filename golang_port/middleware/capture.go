package middleware

import (
	"bytes"
	"io"
	"net/http"
	"time"

	"github.com/labstack/echo/v4"

	"github.com/Bytonomics/cld-gateway/observability"
)

// Capture is UNARY-ONLY exchange capture: it buffers the request body,
// wraps the response writer to tee response bytes into a buffer, runs
// next(c), then appends one observability.Entry once next(c) returns.
//
// This middleware must NEVER be mounted on POST /v1/messages. Wrapping
// c.Response().Writer up front (as this middleware structurally must, to
// see response bytes/status before Append) is only safe when the route's
// handler is guaranteed to write its own single response before returning.
// /v1/messages can instead hand off to the single SSE-writer goroutine in
// core/impl/services/stream_writer.go, which owns c.Response() directly and
// must be the ONLY thing that ever writes to it (ARCHITECTURE_v2.md, "SSE +
// logging"); a second wrapper installed here would double-wrap that writer
// and break the Flusher-forwarding invariant the architecture doc calls
// out. Mount Capture only on routes that are always-unary (count_tokens,
// models, health, auth); handlers/messages.go performs its own equivalent
// logging inline for the unary case instead of relying on this middleware.
func Capture(log observability.ExchangeLog) echo.MiddlewareFunc {
	return func(next echo.HandlerFunc) echo.HandlerFunc {
		return func(c echo.Context) error {
			if log == nil {
				return next(c)
			}

			started := time.Now()

			reqBody, _ := io.ReadAll(c.Request().Body)
			_ = c.Request().Body.Close()
			c.Request().Body = io.NopCloser(bytes.NewReader(reqBody))

			rec := &captureWriter{ResponseWriter: c.Response().Writer, buf: &bytes.Buffer{}}
			c.Response().Writer = rec

			handlerErr := next(c)

			entry := observability.Entry{
				RequestID:       RequestIDFromEcho(c),
				StartedAtUnixMs: started.UnixMilli(),
				DurationMs:      time.Since(started).Milliseconds(),
				Request: observability.HTTPRequestRecord{
					Method:  c.Request().Method,
					URI:     c.Request().RequestURI,
					Headers: observability.RedactHeaders(c.Request().Header),
					Body:    observability.CaptureBody(c.Request().Header.Get(echo.HeaderContentType), reqBody),
				},
				Response: observability.HTTPResponseRecord{
					Status:  c.Response().Status,
					Headers: observability.RedactHeaders(c.Response().Header()),
					Body:    observability.CaptureBody(c.Response().Header().Get(echo.HeaderContentType), rec.buf.Bytes()),
				},
			}
			_ = log.Append(entry)

			return handlerErr
		}
	}
}

// captureWriter tees every Write into buf while still forwarding to the
// underlying http.ResponseWriter, so the wrapped handler's response reaches
// the client unchanged.
type captureWriter struct {
	http.ResponseWriter
	buf *bytes.Buffer
}

func (w *captureWriter) Write(b []byte) (int, error) {
	w.buf.Write(b)
	return w.ResponseWriter.Write(b)
}
