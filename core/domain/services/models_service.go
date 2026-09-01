package services

import (
	"context"

	"github.com/Bytonomics/cld-gateway/core/domain/dto"
)

// ModelsService serves GET /v1/models.
type ModelsService interface {
	List(ctx context.Context) (*dto.ModelList, error)
}
