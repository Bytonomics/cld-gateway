package backend

import (
	"context"
)

type Capabilities struct {
	WebSocketDelta  bool
	ServerSideState bool
}

type SessionKey string

func (s SessionKey) String() string {
	return string(s)
}

type ChainID string

func (c ChainID) String() string {
	return string(c)
}

type Backend interface {
	SendUnary(ctx context.Context, req *Request) (*Response, error)
	SendStream(ctx context.Context, req *Request) (<-chan Event, error)
	Capabilities() Capabilities
	EvictSession(key SessionKey)
	HasLiveSession(key SessionKey) bool
	LiveChainID(key SessionKey) (ChainID, bool)

	// FetchStatusData returns backend-specific status/usage data (e.g. plan
	// type, rate limits, spend control) as a plain JSON-shaped map, for the
	// translated "status" slash command (core/impl/services/
	// translate_executor.go). Each backend implements this against its own
	// status/usage API - the executor calls it through this interface so
	// adding a new backend's status support never requires changing the
	// executor itself.
	FetchStatusData(ctx context.Context) (map[string]any, error)
}
