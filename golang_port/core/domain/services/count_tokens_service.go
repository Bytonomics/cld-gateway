package services

import (
	"context"

	"github.com/Bytonomics/cld-gateway/core/domain/dto"
)

// CountTokensService estimates input token count for a not-yet-sent
// MessagesRequest (POST /v1/messages/count_tokens).
type CountTokensService interface {
	Estimate(ctx context.Context, req *dto.MessagesRequest) int64
}
