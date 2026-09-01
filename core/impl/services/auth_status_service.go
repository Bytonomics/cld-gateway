package services

import (
	"context"

	"github.com/Bytonomics/cld-gateway/core/domain/dto"
	apperr "github.com/Bytonomics/cld-gateway/core/domain/errors"
	authport "github.com/Bytonomics/cld-gateway/core/domain/port/auth"
	"github.com/Bytonomics/cld-gateway/core/domain/services"
)

const authRemediation = "Please run: cld-gateway login claude"

// AuthStatusService implements services.AuthStatusService by delegating to
// an auth.Provider, porting auth_status/auth_status_response and
// auth_refresh (crates/gateway-http-anthropic/src/lib.rs:825-880).
//
// Deviation from the Rust response shape: auth.Provider.Status (the pinned
// port/auth.Provider contract, FILEMAP.md) does not expose
// has_openai_api_key or a GatewayLoginMethod the way
// gateway_auth_codex::GatewayAuthStatus does, so LoginMethod/ReadyForModels
// here are derived from IsLoggedIn (the ChatGPT-login path) rather than
// independently observing API-key mode. Widening auth.Provider.Status to
// carry those fields is out of this task's file list.
type AuthStatusService struct {
	provider authport.Provider
}

var _ services.AuthStatusService = (*AuthStatusService)(nil)

// NewAuthStatusService constructs an AuthStatusService over provider.
func NewAuthStatusService(provider authport.Provider) *AuthStatusService {
	return &AuthStatusService{provider: provider}
}

// Status ports auth_status_response (lib.rs:833-867).
func (s *AuthStatusService) Status(ctx context.Context) (*dto.AuthStatus, error) {
	status, err := s.provider.Status(ctx)
	if err != nil {
		return &dto.AuthStatus{
			LoggedIn:        false,
			Source:          "error",
			ErrorType:       err.Error(),
			AuthRemediation: authRemediation,
		}, nil
	}

	if status == nil || (!status.IsLoggedIn && status.AccountID == "" && !status.HasAccessToken && !status.HasRefreshToken) {
		return &dto.AuthStatus{
			LoggedIn:        false,
			Source:          "gateway_auth_json",
			AuthRemediation: authRemediation,
		}, nil
	}

	out := &dto.AuthStatus{
		LoggedIn:         status.IsLoggedIn,
		ReadyForMessages: status.IsLoggedIn,
		ReadyForModels:   status.IsLoggedIn,
		Source:           "gateway_auth_json",
	}
	if status.AccountID != "" {
		out.AccountID = &status.AccountID
	}
	if status.IsLoggedIn {
		method := "chatgpt"
		out.LoginMethod = &method
	}
	return out, nil
}

// Refresh ports auth_refresh (lib.rs:869-880): refresh-and-persist, then
// report the resulting status.
func (s *AuthStatusService) Refresh(ctx context.Context) (*dto.AuthStatus, error) {
	if _, err := s.provider.RefreshAndPersist(ctx); err != nil {
		return nil, apperr.Wrap(err, apperr.CodeAuthentication, "refresh auth", 401)
	}
	return s.Status(ctx)
}
