package middleware

import (
	"fmt"
	"net/http"
	"net/http/httptest"
	"testing"

	"github.com/labstack/echo/v4"

	apperr "github.com/Bytonomics/cld-gateway/core/domain/errors"
	"github.com/Bytonomics/cld-gateway/observability"
)

type fakeExchangeLog struct {
	lastEntry observability.Entry
	appended  bool
}

func (f *fakeExchangeLog) Append(entry observability.Entry) error {
	f.lastEntry = entry
	f.appended = true
	return nil
}

func newTestEcho() (*echo.Echo, *fakeExchangeLog) {
	e := echo.New()
	e.HTTPErrorHandler = ErrorHandler
	return e, &fakeExchangeLog{}
}

func TestCapture_HandlerReturnsPlainError_LogsRealStatus(t *testing.T) {
	e, log := newTestEcho()
	e.GET("/test", func(c echo.Context) error {
		return fmt.Errorf("boom")
	}, Capture(log))

	req := httptest.NewRequest(http.MethodGet, "/test", nil)
	rec := httptest.NewRecorder()
	e.ServeHTTP(rec, req)

	if !log.appended {
		t.Fatal("log.appended = false, want true")
	}
	if log.lastEntry.Response.Status != 500 {
		t.Errorf("logged Status = %d, want 500", log.lastEntry.Response.Status)
	}
	if log.lastEntry.Response.Status != rec.Code {
		t.Errorf("logged Status (%d) != rec.Code (%d) - the bug this test guards against", log.lastEntry.Response.Status, rec.Code)
	}
}

func TestCapture_HandlerReturnsAppError_LogsRealStatus(t *testing.T) {
	e, log := newTestEcho()
	e.GET("/test", func(c echo.Context) error {
		return apperr.New(apperr.CodeNotFound, "not found", 404)
	}, Capture(log))

	req := httptest.NewRequest(http.MethodGet, "/test", nil)
	rec := httptest.NewRecorder()
	e.ServeHTTP(rec, req)

	if log.lastEntry.Response.Status != 404 {
		t.Errorf("logged Status = %d, want 404", log.lastEntry.Response.Status)
	}
	if rec.Code != 404 {
		t.Errorf("rec.Code = %d, want 404", rec.Code)
	}
	if log.lastEntry.Response.Status != rec.Code {
		t.Errorf("logged Status (%d) != rec.Code (%d)", log.lastEntry.Response.Status, rec.Code)
	}
}

func TestCapture_HandlerReturnsEchoHTTPError_LogsRealStatus(t *testing.T) {
	e, log := newTestEcho()
	e.GET("/test", func(c echo.Context) error {
		return echo.NewHTTPError(http.StatusBadRequest, "bad request")
	}, Capture(log))

	req := httptest.NewRequest(http.MethodGet, "/test", nil)
	rec := httptest.NewRecorder()
	e.ServeHTTP(rec, req)

	if log.lastEntry.Response.Status != 400 {
		t.Errorf("logged Status = %d, want 400", log.lastEntry.Response.Status)
	}
	if rec.Code != 400 {
		t.Errorf("rec.Code = %d, want 400", rec.Code)
	}
	if log.lastEntry.Response.Status != rec.Code {
		t.Errorf("logged Status (%d) != rec.Code (%d)", log.lastEntry.Response.Status, rec.Code)
	}
}

func TestCapture_HandlerSucceeds_LogsRealSuccessStatus(t *testing.T) {
	e, log := newTestEcho()
	e.GET("/test", func(c echo.Context) error {
		return c.JSON(http.StatusOK, map[string]string{"ok": "true"})
	}, Capture(log))

	req := httptest.NewRequest(http.MethodGet, "/test", nil)
	rec := httptest.NewRecorder()
	e.ServeHTTP(rec, req)

	if !log.appended {
		t.Fatal("log.appended = false, want true")
	}
	if log.lastEntry.Response.Status != 200 {
		t.Errorf("logged Status = %d, want 200", log.lastEntry.Response.Status)
	}
	if rec.Code != 200 {
		t.Errorf("rec.Code = %d, want 200", rec.Code)
	}
}

func TestCapture_NilLog_DoesNotPanicAndCallsHandler(t *testing.T) {
	e := echo.New()
	e.HTTPErrorHandler = ErrorHandler
	e.GET("/test", func(c echo.Context) error {
		return c.JSON(http.StatusOK, map[string]string{"ok": "true"})
	}, Capture(nil))

	req := httptest.NewRequest(http.MethodGet, "/test", nil)
	rec := httptest.NewRecorder()
	e.ServeHTTP(rec, req)

	if rec.Code != 200 {
		t.Errorf("rec.Code = %d, want 200", rec.Code)
	}
}
