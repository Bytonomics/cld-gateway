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
}
