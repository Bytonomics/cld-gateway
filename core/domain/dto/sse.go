package dto

// SSEEvent is one Anthropic-shaped server-sent event ready to write to the
// response stream: "event: <Event>\ndata: <Data>\n\n".
type SSEEvent struct {
	Event string
	Data  []byte
}
