package middleware

import (
	"errors"
	"fmt"
	"net/http"

	"github.com/labstack/echo/v4"

	apperr "github.com/Bytonomics/cld-gateway/core/domain/errors"
)

// Recover catches panics from any handler/middleware further down the
// chain and hands the recovered value to Echo's centralized error path
// (c.Error, which invokes e.HTTPErrorHandler - see ErrorHandler below) so
// panics render as the same AppError->Anthropic JSON shape as any other
// error, rather than an ad-hoc response write here.
func Recover() echo.MiddlewareFunc {
	return func(next echo.HandlerFunc) echo.HandlerFunc {
		return func(c echo.Context) error {
			defer func() {
				if r := recover(); r != nil {
					if r == http.ErrAbortHandler {
						panic(r)
					}
					err, ok := r.(error)
					if !ok {
						err = fmt.Errorf("%v", r)
					}
					c.Error(apperr.Wrap(err, apperr.CodeAPI, "internal error", http.StatusInternalServerError))
				}
			}()
			return next(c)
		}
	}
}

// ErrorHandler is the central Echo HTTPErrorHandler (ARCHITECTURE_v2.md
// "Error model"): every error reaching Echo's error path - handler errors,
// pedantigoecho binder validation failures (echo.HTTPError), and panics
// recovered by Recover() above - is serialized here as the Anthropic error
// shape via errors/anthropic.go. app/router.go (a later wave) wires this in
// with `e.HTTPErrorHandler = middleware.ErrorHandler`.
func ErrorHandler(err error, c echo.Context) {
	if c.Response().Committed {
		return
	}

	appErr := toAppError(err)
	status := appErr.HTTPStatus
	if status == 0 {
		status = http.StatusInternalServerError
	}

	payload := apperr.AnthropicPayload(appErr)

	var writeErr error
	if c.Request().Method == http.MethodHead {
		writeErr = c.NoContent(status)
	} else {
		writeErr = c.JSON(status, payload)
	}
	if writeErr != nil {
		c.Logger().Error(writeErr)
	}
}

// toAppError normalizes any error reaching ErrorHandler into an *AppError.
// echo.HTTPError (the shape pedantigoecho's binder returns on validation
// failure - see plugins/web/pedantigoecho) maps to invalid_request_error
// per ARCHITECTURE_v2.md's "Pedantigo field-path validation errors map to
// invalid_request_error with paths in the message."
func toAppError(err error) *apperr.AppError {
	var appErr *apperr.AppError
	if errors.As(err, &appErr) {
		return appErr
	}

	var httpErr *echo.HTTPError
	if errors.As(err, &httpErr) {
		return apperr.New(apperr.CodeInvalidRequest, fmt.Sprintf("%v", httpErr.Message), httpErr.Code)
	}

	return apperr.Wrap(err, apperr.CodeAPI, err.Error(), http.StatusInternalServerError)
}
