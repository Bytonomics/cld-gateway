package services

import (
	"context"

	"github.com/Bytonomics/cld-gateway/core/domain/dto"
)

// AuthStatusService serves GET /auth/status and POST /auth/refresh.
type AuthStatusService interface {
	Status(ctx context.Context) (*dto.AuthStatus, error)
	Refresh(ctx context.Context) (*dto.AuthStatus, error)
}
