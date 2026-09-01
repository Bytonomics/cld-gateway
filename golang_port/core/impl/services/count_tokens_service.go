package services

import (
	"context"
	"encoding/json"

	"github.com/Bytonomics/cld-gateway/core/domain/dto"
	"github.com/Bytonomics/cld-gateway/core/domain/services"
)

// CountTokensService implements services.CountTokensService, porting
// estimate_anthropic_count_tokens (crates/gateway-http-anthropic/src/lib.rs:914-920):
// ceil(len(json-reencoded request)/4). This is a structural size estimate
// over the already-parsed, already-validated request - not prompt-text
// sniffing for intent - so it does not fall under the CLAUDE.md prompt-text
// rule.
type CountTokensService struct{}

var _ services.CountTokensService = CountTokensService{}

// NewCountTokensService constructs a CountTokensService.
func NewCountTokensService() CountTokensService {
	return CountTokensService{}
}

// Estimate re-encodes req to JSON and returns ceil(len(encoded)/4), mirroring
// lib.rs's div_ceil(4) over the re-serialized request body.
func (CountTokensService) Estimate(_ context.Context, req *dto.MessagesRequest) int64 {
	encoded, err := json.Marshal(req)
	if err != nil {
		return 0
	}
	return int64((len(encoded) + 3) / 4)
}
