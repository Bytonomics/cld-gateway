// Package codexauth implements core/domain/port/auth.Provider by reading
// and writing the gateway auth.json file (Codex OAuth tokens or an OpenAI
// API key), matching crates/gateway-auth-codex.
package codexauth

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/Bytonomics/cld-gateway/core"
	"github.com/Bytonomics/cld-gateway/core/domain/port/auth"
	"github.com/Bytonomics/cld-gateway/netpolicy"
)

var (
	ErrAuthNotFound  = errors.New("no gateway auth found")
	ErrJWTMalformed  = errors.New("access token is not a JWT")
	ErrJWTDecode     = errors.New("failed to decode JWT payload")
	ErrJWTClaimsJSON = errors.New("failed to parse JWT claims JSON")
)

func errMissingField(field string) error {
	return fmt.Errorf("auth.json missing required field: %s", field)
}

type authJSON struct {
	AuthMode     *string `json:"auth_mode,omitempty"`
	OpenAIAPIKey *string `json:"OPENAI_API_KEY,omitempty"`
	Tokens       *tokens `json:"tokens,omitempty"`
	LastRefresh  *string `json:"last_refresh,omitempty"`
}

type tokens struct {
	AccessToken  *string `json:"access_token,omitempty"`
	RefreshToken *string `json:"refresh_token,omitempty"`
	IDToken      *string `json:"id_token,omitempty"`
	AccountID    *string `json:"account_id,omitempty"`
}

// DefaultAuthJSONPath ports paths::default_auth_json_path: GATEWAY_AUTH_JSON_PATH
// (full path) wins; else GATEWAY_HOME/auth.json; else ~/.gateway/auth.json.
func DefaultAuthJSONPath() string {
	if p := os.Getenv("GATEWAY_AUTH_JSON_PATH"); p != "" {
		return p
	}
	if home := os.Getenv("GATEWAY_HOME"); home != "" {
		return filepath.Join(home, "auth.json")
	}
	homeDir, err := os.UserHomeDir()
	if err != nil {
		homeDir = "."
	}
	return filepath.Join(homeDir, ".gateway", "auth.json")
}

// extractExpUnverified ports jwt::extract_exp_unverified.
func extractExpUnverified(accessToken string) (*int64, error) {
	parts := strings.Split(accessToken, ".")
	if len(parts) < 2 {
		return nil, ErrJWTMalformed
	}
	payload, err := base64.RawURLEncoding.DecodeString(parts[1])
	if err != nil {
		return nil, ErrJWTDecode
	}
	var claims struct {
		Exp *int64 `json:"exp"`
	}
	if err := json.Unmarshal(payload, &claims); err != nil {
		return nil, ErrJWTClaimsJSON
	}
	return claims.Exp, nil
}

// extractChatGPTAccountIDUnverified ports jwt::extract_chatgpt_account_id_unverified.
func extractChatGPTAccountIDUnverified(idToken string) (string, bool) {
	parts := strings.Split(idToken, ".")
	if len(parts) < 2 {
		return "", false
	}
	payload, err := base64.RawURLEncoding.DecodeString(parts[1])
	if err != nil {
		return "", false
	}
	var claims map[string]any
	if err := json.Unmarshal(payload, &claims); err != nil {
		return "", false
	}
	if nested, ok := claims["https://api.openai.com/auth"].(map[string]any); ok {
		if v, ok := nested["chatgpt_account_id"].(string); ok {
			return v, true
		}
	}
	if v, ok := claims["chatgpt_account_id"].(string); ok {
		return v, true
	}
	return "", false
}

// Store implements auth.Provider against a single auth.json file.
type Store struct {
	path string
	http *netpolicy.Client
}

var _ auth.Provider = (*Store)(nil)

// New builds a Store reading/writing the given auth.json path.
func New(path string) *Store {
	return &Store{path: path, http: netpolicy.New(nil)}
}

// NewDefault builds a Store against DefaultAuthJSONPath().
func NewDefault() *Store {
	return &Store{path: DefaultAuthJSONPath(), http: netpolicy.New(nil)}
}

// httpClient returns the Store's outbound HTTP client, lazily defaulting it
// for Store values built without the New/NewDefault constructors.
func (s *Store) httpClient() *netpolicy.Client {
	if s.http == nil {
		s.http = netpolicy.New(nil)
	}
	return s.http
}

func (s *Store) loadAuthJSON() (*authJSON, error) {
	raw, err := os.ReadFile(s.path)
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return nil, fmt.Errorf("%w at %s", ErrAuthNotFound, s.path)
		}
		return nil, fmt.Errorf("failed to read auth.json at %s: %w", s.path, err)
	}
	var parsed authJSON
	if err := json.Unmarshal(raw, &parsed); err != nil {
		return nil, fmt.Errorf("failed to parse auth.json: %w", err)
	}
	return &parsed, nil
}

// AccessToken ports load_access_token_default_path (via load_credentials).
func (s *Store) AccessToken(ctx context.Context) (core.Secret, error) {
	parsed, err := s.loadAuthJSON()
	if err != nil {
		return "", err
	}
	if parsed.Tokens == nil {
		return "", errMissingField("tokens")
	}
	if parsed.Tokens.AccessToken == nil {
		return "", errMissingField("tokens.access_token")
	}
	return core.NewSecret(*parsed.Tokens.AccessToken), nil
}

// AccountID ports load_credentials's account_id resolution (tokens.account_id,
// falling back to the id_token JWT claim).
func (s *Store) AccountID(ctx context.Context) (string, error) {
	parsed, err := s.loadAuthJSON()
	if err != nil {
		return "", err
	}
	if parsed.Tokens == nil {
		return "", errMissingField("tokens")
	}
	if parsed.Tokens.AccountID != nil {
		return *parsed.Tokens.AccountID, nil
	}
	if parsed.Tokens.IDToken != nil {
		if id, ok := extractChatGPTAccountIDUnverified(*parsed.Tokens.IDToken); ok {
			return id, nil
		}
	}
	return "", errMissingField("tokens.account_id")
}

// RefreshAndPersist ports CodexAuthManager::refresh_and_persist
// (crates/gateway-auth-codex/src/lib.rs:441-499): performs the refresh_token
// grant against the Codex token endpoint and atomically persists the
// updated tokens back to auth.json.
func (s *Store) RefreshAndPersist(ctx context.Context) (auth.Snapshot, error) {
	parsed, err := s.loadAuthJSON()
	if err != nil {
		return auth.Snapshot{}, err
	}
	if parsed.Tokens == nil {
		return auth.Snapshot{}, errMissingField("tokens")
	}
	if parsed.Tokens.RefreshToken == nil || *parsed.Tokens.RefreshToken == "" {
		return auth.Snapshot{}, errMissingField("tokens.refresh_token")
	}

	refreshed, err := RefreshAccessToken(ctx, s.httpClient(), defaultTokenURL, defaultClientID, *parsed.Tokens.RefreshToken)
	if err != nil {
		return auth.Snapshot{}, err
	}
	if refreshed.AccessToken == nil {
		return auth.Snapshot{}, fmt.Errorf("%w: missing access_token", ErrRefreshUnexpectedResponse)
	}

	parsed.Tokens.AccessToken = refreshed.AccessToken
	if refreshed.RefreshToken != nil {
		parsed.Tokens.RefreshToken = refreshed.RefreshToken
	}
	if refreshed.IDToken != nil {
		parsed.Tokens.IDToken = refreshed.IDToken
	}

	if err := s.atomicWriteJSON(parsed); err != nil {
		return auth.Snapshot{}, err
	}

	accountID, err := s.AccountID(ctx)
	if err != nil {
		return auth.Snapshot{}, err
	}

	var expiresAt *int64
	if exp, expErr := extractExpUnverified(*parsed.Tokens.AccessToken); expErr == nil {
		expiresAt = exp
	}

	return auth.Snapshot{
		AccountID:            accountID,
		HasAccessToken:       true,
		HasRefreshToken:      parsed.Tokens.RefreshToken != nil && *parsed.Tokens.RefreshToken != "",
		ExpiresAtUnixSeconds: expiresAt,
	}, nil
}

// Status ports auth_status_from_parsed + GatewayAuthStatus::ready_for_messages
// (lib.rs:145-197,46-53). IsLoggedIn mirrors ready_for_messages: ChatGPT login
// method (no OPENAI_API_KEY present) with an access token, a refresh token,
// and a resolved account id. A missing auth.json is reported as a logged-out
// status rather than an error, mirroring load_gateway_auth_status's
// AuthNotFound -> Ok(None) mapping.
func (s *Store) Status(ctx context.Context) (*auth.Status, error) {
	parsed, err := s.loadAuthJSON()
	if err != nil {
		if errors.Is(err, ErrAuthNotFound) {
			return &auth.Status{}, nil
		}
		return nil, err
	}

	hasOpenAIAPIKey := parsed.OpenAIAPIKey != nil

	var hasAccessToken, hasRefreshToken bool
	var accessToken, accountID string
	if parsed.Tokens != nil {
		hasAccessToken = parsed.Tokens.AccessToken != nil
		hasRefreshToken = parsed.Tokens.RefreshToken != nil
		if parsed.Tokens.AccessToken != nil {
			accessToken = *parsed.Tokens.AccessToken
		}
		if parsed.Tokens.AccountID != nil {
			accountID = *parsed.Tokens.AccountID
		} else if parsed.Tokens.IDToken != nil {
			if id, ok := extractChatGPTAccountIDUnverified(*parsed.Tokens.IDToken); ok {
				accountID = id
			}
		}
	}

	isChatGPTLogin := !hasOpenAIAPIKey
	isLoggedIn := isChatGPTLogin && hasAccessToken && hasRefreshToken && accountID != ""

	var expiresAt *int64
	if hasAccessToken {
		if exp, err := extractExpUnverified(accessToken); err == nil {
			expiresAt = exp
		}
	}

	return &auth.Status{
		AccountID:            accountID,
		HasAccessToken:       hasAccessToken,
		HasRefreshToken:      hasRefreshToken,
		IsLoggedIn:           isLoggedIn,
		ExpiresAtUnixSeconds: expiresAt,
	}, nil
}

// Logout ports revoke::logout / logout_with_revoke
// (crates/gateway-auth-codex/src/revoke.rs:44-69): when revoke is set, it
// best-effort revokes the account's refresh (preferred) or access token
// before removing auth.json. A missing auth.json is not an error.
func (s *Store) Logout(ctx context.Context, revoke bool) error {
	if revoke {
		s.revokeTokens(ctx)
	}

	err := os.Remove(s.path)
	if err != nil {
		if errors.Is(err, os.ErrNotExist) {
			return nil
		}
		return fmt.Errorf("failed to remove auth.json at %s: %w", s.path, err)
	}
	return nil
}

// LoadOpenAIAPIKey ports load_openai_api_key_default_path (lib.rs:270-279).
// ok is false (with nil error) when auth.json has no OPENAI_API_KEY.
func (s *Store) LoadOpenAIAPIKey(ctx context.Context) (key core.Secret, ok bool, err error) {
	parsed, err := s.loadAuthJSON()
	if err != nil {
		return "", false, err
	}
	if parsed.OpenAIAPIKey == nil {
		return "", false, nil
	}
	return core.NewSecret(*parsed.OpenAIAPIKey), true, nil
}

// WriteOpenAIAPIKey ports write_openai_api_key_default_path (lib.rs:281-299):
// it replaces auth.json wholesale with an API-key-mode document.
func (s *Store) WriteOpenAIAPIKey(ctx context.Context, apiKey string) error {
	authMode := "api_key"
	doc := authJSON{
		AuthMode:     &authMode,
		OpenAIAPIKey: &apiKey,
	}
	return s.atomicWriteJSON(&doc)
}

// atomicWriteJSON ports persist::atomic_write_json: write to a temp file in
// the target directory, then rename over the destination.
func (s *Store) atomicWriteJSON(v any) error {
	dir := filepath.Dir(s.path)
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return err
	}

	bytes, err := json.MarshalIndent(v, "", "  ")
	if err != nil {
		return err
	}

	tmp, err := os.CreateTemp(dir, ".auth-*.tmp")
	if err != nil {
		return err
	}
	tmpPath := tmp.Name()

	if _, err := tmp.Write(bytes); err != nil {
		_ = tmp.Close()
		_ = os.Remove(tmpPath)
		return err
	}
	if err := tmp.Close(); err != nil {
		_ = os.Remove(tmpPath)
		return err
	}
	if err := os.Rename(tmpPath, s.path); err != nil {
		_ = os.Remove(tmpPath)
		return err
	}
	return nil
}
