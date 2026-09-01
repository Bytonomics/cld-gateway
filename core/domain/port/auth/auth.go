package auth

import (
	"context"

	"github.com/Bytonomics/cld-gateway/core"
)

// Snapshot is a local mirror (no codexauth dependency)
type Snapshot struct {
	AccountID            string
	HasAccessToken       bool
	HasRefreshToken      bool
	ExpiresAtUnixSeconds *int64
}

// Status is a local mirror
type Status struct {
	AccountID            string
	HasAccessToken       bool
	HasRefreshToken      bool
	IsLoggedIn           bool
	ExpiresAtUnixSeconds *int64
}

type Provider interface {
	AccessToken(ctx context.Context) (core.Secret, error)
	AccountID(ctx context.Context) (string, error)
	RefreshAndPersist(ctx context.Context) (Snapshot, error)
	Status(ctx context.Context) (*Status, error)
	Logout(ctx context.Context, revoke bool) error
}
