package codexauth

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strings"

	"github.com/Bytonomics/cld-gateway/netpolicy"
)

const (
	defaultTokenURL = "https://auth.openai.com/oauth/token"
	defaultClientID = "app_EMoamEEZ73f0CkXaXp7hrann"
)

var (
	ErrRefreshTransportFailed    = errors.New("token refresh transport failed")
	ErrRefreshFailed             = errors.New("token refresh failed")
	ErrRefreshUnauthorized       = errors.New("token refresh rejected as unauthorized")
	ErrRefreshUnexpectedResponse = errors.New("token endpoint returned unexpected response")
)

// RefreshUnauthorizedError wraps ErrRefreshUnauthorized (permanent refresh
// failure per HTTP 401 from the token endpoint, so callers can trigger
// logout rather than retry) and carries the upstream error code/body.
type RefreshUnauthorizedError struct {
	Code string
	Body string
}

func (e *RefreshUnauthorizedError) Error() string {
	if e.Code != "" {
		return fmt.Sprintf("token refresh rejected with code %q: %s", e.Code, e.Body)
	}
	return fmt.Sprintf("token refresh rejected: %s", e.Body)
}

func (e *RefreshUnauthorizedError) Unwrap() error { return ErrRefreshUnauthorized }

// RefreshResponse ports oauth::RefreshResponse.
type RefreshResponse struct {
	AccessToken  *string `json:"access_token"`
	RefreshToken *string `json:"refresh_token"`
	IDToken      *string `json:"id_token"`
}

// RefreshAccessToken performs an OAuth 2.0 refresh_token grant against the
// Codex token endpoint. Ports oauth::refresh_access_token
// (crates/gateway-auth-codex/src/oauth.rs:27-96). A 401 response is a
// permanent auth failure (*RefreshUnauthorizedError) so callers can trigger
// logout instead of retrying.
func RefreshAccessToken(ctx context.Context, httpClient *netpolicy.Client, tokenURL, clientID, refreshToken string) (*RefreshResponse, error) {
	form := url.Values{
		"grant_type":    {"refresh_token"},
		"client_id":     {clientID},
		"refresh_token": {refreshToken},
	}

	req, err := http.NewRequestWithContext(ctx, http.MethodPost, tokenURL, strings.NewReader(form.Encode()))
	if err != nil {
		return nil, fmt.Errorf("%w: %v", ErrRefreshTransportFailed, err)
	}
	req.Header.Set("Content-Type", "application/x-www-form-urlencoded")

	res, err := httpClient.Do(req)
	if err != nil {
		return nil, fmt.Errorf("%w: %v", ErrRefreshTransportFailed, err)
	}
	defer func() { _ = res.Body.Close() }()

	bodyBytes, err := io.ReadAll(res.Body)
	if err != nil {
		return nil, fmt.Errorf("%w: %v", ErrRefreshUnexpectedResponse, err)
	}
	body := string(bodyBytes)

	if res.StatusCode < 200 || res.StatusCode >= 300 {
		if res.StatusCode == http.StatusUnauthorized {
			return nil, &RefreshUnauthorizedError{Code: refreshErrorCode(body), Body: body}
		}
		return nil, fmt.Errorf("%w with status %d: %s", ErrRefreshFailed, res.StatusCode, body)
	}

	var parsed RefreshResponse
	if err := json.Unmarshal(bodyBytes, &parsed); err != nil {
		return nil, fmt.Errorf("%w: %s", ErrRefreshUnexpectedResponse, body)
	}
	if parsed.AccessToken == nil {
		return nil, fmt.Errorf("%w: %s", ErrRefreshUnexpectedResponse, body)
	}

	return &parsed, nil
}

// refreshErrorCode ports oauth::refresh_error_code: extracts an upstream
// error code from a token-endpoint error body, whether "error" is an object
// with a "code" field, a bare string, or absent (in which case a top-level
// "code" field is tried).
func refreshErrorCode(body string) string {
	var value map[string]any
	if err := json.Unmarshal([]byte(body), &value); err != nil {
		return ""
	}

	errField, ok := value["error"]
	if !ok {
		if code, ok := value["code"].(string); ok {
			return code
		}
		return ""
	}

	switch e := errField.(type) {
	case map[string]any:
		if code, ok := e["code"].(string); ok {
			return code
		}
	case string:
		return e
	}

	if code, ok := value["code"].(string); ok {
		return code
	}
	return ""
}
