package dto

// CountTokensResponse is the POST /v1/messages/count_tokens response body,
// ported from v1_messages_count_tokens (lib.rs:891-912). The estimate is
// computed as ceil(len(json-reencoded-body) / 4) by CountTokensService.
type CountTokensResponse struct {
	InputTokens int64 `json:"input_tokens"`
}
