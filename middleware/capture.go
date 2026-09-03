package middleware

import (
	"bytes"
	"io"
	"net/http"
	"time"

	"github.com/labstack/echo/v4"

	"github.com/Bytonomics/cld-gateway/observability"
)

// Capture is exchange capture for both unary and streaming routes: it
// buffers the request body, wraps the response writer to tee response bytes
// into a buffer, runs next(c), then appends one observability.Entry once
// next(c) returns.
//
// Safe on streaming routes (including POST /v1/messages) because
// captureWriter implements Unwrap() http.ResponseWriter (Go 1.20+,
// http.ResponseController) - echo.Response.Flush() resolves flushing through
// http.NewResponseController(r.Writer).Flush(), which follows Unwrap() to
// find the real underlying http.Flusher when the immediate writer doesn't
// implement it directly. This supersedes ADR-0004's "no middleware may wrap
// the response writer on streaming routes" constraint - see ADR-0013.
// core/impl/services/stream_writer.go's StreamWriter.Run is still the sole
// writer of SSE bytes to c.Response() for the duration of one streaming
// exchange (unchanged; that invariant was never about middleware, it's
// about not racing two goroutines on the same writer), and handlers/
// messages.go calls it synchronously, so Capture's post-next(c) logging
// still runs strictly after the last byte reaches the client - Option C
// logging semantics are preserved, just from the middleware side instead of
// from inside StreamWriter itself.
//
// Handlers set per-request log metadata (e.g. dto.MessagesResponse's
// ContextManagementMetadata) via c.Set(exchangeMetadataContextKey, v) before
// returning; Capture reads it back after next(c) to populate Entry.Metadata.
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

			if handlerErr != nil && !c.Response().Committed {
				status, payload := ClassifyForResponse(handlerErr)
				if c.Request().Method == http.MethodHead {
					_ = c.NoContent(status)
				} else {
					_ = c.JSON(status, payload)
				}
			}

			var metadata map[string]any
			if v, ok := c.Get(exchangeMetadataContextKey).(map[string]any); ok {
				metadata = v
			}

			entry := observability.Entry{
				RequestID:       RequestIDFromEcho(c),
				StartedAtUnixMs: started.UnixMilli(),
				DurationMs:      time.Since(started).Milliseconds(),
				Metadata:        metadata,
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

			if handlerErr != nil && !c.Response().Committed {
				return nil
			}
			return handlerErr
		}
	}
}

// exchangeMetadataContextKey is the echo.Context key a handler sets via
// c.Set before returning, carrying per-request data (e.g.
// dto.MessagesResponse.ContextManagementMetadata) that Capture folds into
// the logged Entry.Metadata after next(c) returns.
const exchangeMetadataContextKey = "exchange_metadata"

// captureWriter tees every Write into buf while still forwarding to the
// underlying http.ResponseWriter, so the wrapped handler's response reaches
// the client unchanged.
//
// Implements Unwrap() http.ResponseWriter so http.ResponseController (which
// echo.Response.Flush()/Hijack() use internally) can find the real
// underlying http.Flusher/http.Hijacker through this wrapper - this is what
// makes it safe to mount on streaming routes (see the Capture doc comment
// and ADR-0013).
type captureWriter struct {
	http.ResponseWriter
	buf *bytes.Buffer
}

func (w *captureWriter) Write(b []byte) (int, error) {
	w.buf.Write(b)
	return w.ResponseWriter.Write(b)
}

func (w *captureWriter) Unwrap() http.ResponseWriter {
	return w.ResponseWriter
}
