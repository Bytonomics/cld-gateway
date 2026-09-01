package codexauth

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"time"

	"github.com/Bytonomics/cld-gateway/netpolicy"
)

const (
	revokeTokenURL    = "https://auth.openai.com/oauth/revoke"
	revokeHTTPTimeout = 10 * time.Second
	revokeRefreshCID  = "app_EMoamEEZ73f0CkXaXp7hrann"
)

type revokeTokenKind int

const (
	revokeAccessToken revokeTokenKind = iota
	revokeRefreshToken
)

func (k revokeTokenKind) hint() string {
	switch k {
	case revokeRefreshToken:
		return "refresh_token"
	default:
		return "access_token"
	}
}

func (k revokeTokenKind) clientID() string {
	if k == revokeRefreshToken {
		return revokeRefreshCID
	}
	return ""
}

type revokeTokenRequest struct {
	Token         string `json:"token"`
	TokenTypeHint string `json:"token_type_hint"`
	ClientID      string `json:"client_id,omitempty"`
}

// revocableToken ports revoke::revocable_token: prefers a non-empty refresh
// token, falling back to a non-empty access token.
func revocableToken(t *tokens) (token string, kind revokeTokenKind, ok bool) {
	if t == nil {
		return "", 0, false
	}
	if t.RefreshToken != nil && *t.RefreshToken != "" {
		return *t.RefreshToken, revokeRefreshToken, true
	}
	if t.AccessToken != nil && *t.AccessToken != "" {
		return *t.AccessToken, revokeAccessToken, true
	}
	return "", 0, false
}

// revokeOAuthToken ports revoke::revoke_oauth_token.
func revokeOAuthToken(ctx context.Context, httpClient *netpolicy.Client, endpoint, token string, kind revokeTokenKind) error {
	payload := revokeTokenRequest{
		Token:         token,
		TokenTypeHint: kind.hint(),
		ClientID:      kind.clientID(),
	}
	body, err := json.Marshal(payload)
	if err != nil {
		return err
	}

	ctx, cancel := context.WithTimeout(ctx, revokeHTTPTimeout)
	defer cancel()

	req, err := http.NewRequestWithContext(ctx, http.MethodPost, endpoint, bytes.NewReader(body))
	if err != nil {
		return err
	}
	req.Header.Set("Content-Type", "application/json")

	res, err := httpClient.Do(req)
	if err != nil {
		return err
	}
	defer func() { _ = res.Body.Close() }()

	if res.StatusCode >= 200 && res.StatusCode < 300 {
		return nil
	}
	respBody, _ := io.ReadAll(res.Body)
	return fmt.Errorf("failed to revoke %s: %s: %s", kind.hint(), res.Status, string(respBody))
}

// revokeTokens ports logout_with_revoke's revocation step: best-effort
// revocation of the account's refresh (preferred) or access token. Unlike
// the Rust implementation, which silently discards revocation failures via
// tracing::warn, this logs the error through slog so cleanup failures remain
// observable (approved gap fix G5).
func (s *Store) revokeTokens(ctx context.Context) {
	parsed, err := s.loadAuthJSON()
	if err != nil {
		if !errors.Is(err, ErrAuthNotFound) {
			slog.Warn("failed to load auth.json for token revocation", "error", err)
		}
		return
	}

	token, kind, ok := revocableToken(parsed.Tokens)
	if !ok {
		return
	}

	if err := revokeOAuthToken(ctx, s.httpClient(), revokeTokenURL, token, kind); err != nil {
		slog.Warn("failed to revoke auth tokens during logout", "error", err)
	}
}
