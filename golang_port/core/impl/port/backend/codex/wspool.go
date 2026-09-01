package codex

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/url"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/coder/websocket"
	"github.com/google/uuid"

	portauth "github.com/Bytonomics/cld-gateway/core/domain/port/auth"
	"github.com/Bytonomics/cld-gateway/core/domain/port/backend"
	"github.com/Bytonomics/cld-gateway/netpolicy"
)

// KeepaliveInterval mirrors WEBSOCKET_KEEPALIVE_INTERVAL
// (websocket_transport.rs:133).
//
// TODO(G10, open gap): hardcoded per the approved default disposition; make
// configurable later.
const KeepaliveInterval = 20 * time.Second

const pingTimeout = 5 * time.Second

const wsResultBuffer = 32

// WSErrorKind classifies a WSTransportError, mirroring the BackendError enum
// variants websocket_transport.rs's retry classification inspects
// (client.rs:22-49).
type WSErrorKind int

const (
	WSErrWebSocket WSErrorKind = iota
	WSErrUnexpectedStatus
	WSErrUnexpectedStatusWithBody
	WSErrRequestFailed
	WSErrAuthFailed
	WSErrNetworkPolicy
)

// WSTransportError is the pooled-WebSocket-transport error shape, mirroring
// the subset of client.rs's BackendError used by websocket_transport.rs's
// retry matchers.
type WSTransportError struct {
	Kind    WSErrorKind
	Stage   string
	Message string
	Status  int
	Body    string
}

func (e *WSTransportError) Error() string {
	switch e.Kind {
	case WSErrWebSocket:
		return fmt.Sprintf("websocket failed during %s: %s", e.Stage, e.Message)
	case WSErrUnexpectedStatus:
		return fmt.Sprintf("unexpected response status %d", e.Status)
	case WSErrUnexpectedStatusWithBody:
		return fmt.Sprintf("unexpected response status %d: %s", e.Status, e.Body)
	case WSErrRequestFailed:
		return fmt.Sprintf("request failed during %s: %s", e.Stage, e.Message)
	case WSErrAuthFailed:
		return fmt.Sprintf("authentication failed during %s: %s", e.Stage, e.Message)
	case WSErrNetworkPolicy:
		return fmt.Sprintf("outbound request blocked during %s: %s", e.Stage, e.Message)
	default:
		return "websocket transport error"
	}
}

// websocketFailed mirrors websocket_failed (client.rs:340-349). Unlike the
// Rust version, which extracts a status code by scanning the error's Display
// string for known status numbers (tungstenite does not expose the failed
// handshake's status directly), coder/websocket's Dial returns the real
// *http.Response on a non-101 handshake response, so the status is read
// directly from it instead of pattern-matching text.
func websocketFailed(stage string, resp *http.Response, err error) *WSTransportError {
	if resp != nil && resp.StatusCode != 0 && resp.StatusCode != http.StatusSwitchingProtocols {
		return &WSTransportError{Kind: WSErrUnexpectedStatus, Status: resp.StatusCode}
	}
	return &WSTransportError{Kind: WSErrWebSocket, Stage: stage, Message: err.Error()}
}

// websocketMessage mirrors websocket_message (client.rs:351-359).
func websocketMessage(stage string, err error) *WSTransportError {
	return &WSTransportError{Kind: WSErrWebSocket, Stage: stage, Message: err.Error()}
}

// classifyReadError mirrors the "websocket closed" message Rust assigns
// explicitly for Message::Close (websocket_transport.rs:544-551): a graceful
// close frame is detected via websocket.CloseStatus so the retry matchers'
// "closed" substring check keeps working, since Go's CloseError.Error() text
// does not itself contain that word.
func classifyReadError(err error) *WSTransportError {
	if websocket.CloseStatus(err) != -1 {
		return &WSTransportError{Kind: WSErrWebSocket, Stage: "read event", Message: "websocket closed"}
	}
	return websocketMessage("read event", err)
}

// ErrorVariant mirrors WebSocketErrorVariant (websocket_transport.rs:100-106).
type ErrorVariant int

const (
	VariantAny ErrorVariant = iota
	VariantWebSocket
	VariantUnexpectedStatus
	VariantUnexpectedStatusWithBody
)

func (v ErrorVariant) matches(err *WSTransportError) bool {
	switch v {
	case VariantAny:
		return true
	case VariantWebSocket:
		return err.Kind == WSErrWebSocket
	case VariantUnexpectedStatus:
		return err.Kind == WSErrUnexpectedStatus
	case VariantUnexpectedStatusWithBody:
		return err.Kind == WSErrUnexpectedStatusWithBody
	default:
		return false
	}
}

// RetryMatcher mirrors WebSocketErrorMatcher (websocket_transport.rs:92-98).
// Fields are pointers to preserve Rust's Option semantics (unset vs. a
// legitimately empty/zero constraint).
type RetryMatcher struct {
	Variant         ErrorVariant
	Stage           *string
	MessageContains *string
	Status          *int
}

func (m RetryMatcher) matches(err *WSTransportError) bool {
	if !m.Variant.matches(err) {
		return false
	}
	if m.Stage != nil {
		if err.Kind != WSErrWebSocket || err.Stage != *m.Stage {
			return false
		}
	}
	if m.Status != nil {
		switch err.Kind {
		case WSErrUnexpectedStatus, WSErrUnexpectedStatusWithBody:
			if err.Status != *m.Status {
				return false
			}
		default:
			return false
		}
	}
	if m.MessageContains != nil {
		needle := strings.ToLower(*m.MessageContains)
		if !strings.Contains(backendErrorText(err), needle) {
			return false
		}
	}
	return true
}

// backendErrorText mirrors backend_error_text (websocket_transport.rs:758-771).
func backendErrorText(err *WSTransportError) string {
	switch err.Kind {
	case WSErrUnexpectedStatusWithBody:
		return strings.ToLower(err.Body)
	case WSErrUnexpectedStatus:
		return strconv.Itoa(err.Status)
	case WSErrWebSocket, WSErrRequestFailed, WSErrAuthFailed, WSErrNetworkPolicy:
		return strings.ToLower(err.Message)
	default:
		return ""
	}
}

// RetryPolicy mirrors WebSocketRetryPolicy (websocket_transport.rs:61-90).
type RetryPolicy struct {
	MaxRecycles  int
	Retryable    []RetryMatcher
	NonRetryable []RetryMatcher
}

// DefaultRetryPolicy mirrors WebSocketRetryPolicy::default (:68-76).
func DefaultRetryPolicy() RetryPolicy {
	return RetryPolicy{MaxRecycles: 1}
}

func (p RetryPolicy) WithRetryable(m RetryMatcher) RetryPolicy {
	p.Retryable = append(p.Retryable, m)
	return p
}

func (p RetryPolicy) WithNonRetryable(m RetryMatcher) RetryPolicy {
	p.NonRetryable = append(p.NonRetryable, m)
	return p
}

// shouldRecycleWebSocketError mirrors should_recycle_websocket_error
// (:621-636).
func shouldRecycleWebSocketError(err *WSTransportError, policy RetryPolicy) bool {
	for _, m := range policy.NonRetryable {
		if m.matches(err) {
			return false
		}
	}
	if isDefaultNonRetryableError(err) {
		return false
	}
	for _, m := range policy.Retryable {
		if m.matches(err) {
			return true
		}
	}
	return isDefaultRetryableError(err)
}

// isTransportLifecycleError mirrors is_transport_lifecycle_error (:638-640).
func isTransportLifecycleError(err *WSTransportError) bool {
	return isDefaultRetryableError(err)
}

var retryableWebSocketStages = map[string]struct{}{
	"connect":               {},
	"queue response.create": {},
	"send response.create":  {},
	"reply pong":            {},
	"read event":            {},
}

// isDefaultRetryableError mirrors is_default_retryable_error (:642-660).
func isDefaultRetryableError(err *WSTransportError) bool {
	switch err.Kind {
	case WSErrWebSocket:
		if _, ok := retryableWebSocketStages[err.Stage]; !ok {
			return false
		}
		return retryableWebSocketMessage(err.Message)
	case WSErrUnexpectedStatus:
		return retryableStatus(err.Status)
	case WSErrUnexpectedStatusWithBody:
		return retryableStatus(err.Status)
	default:
		return false
	}
}

// isDefaultNonRetryableError mirrors is_default_non_retryable_error
// (:662-677).
func isDefaultNonRetryableError(err *WSTransportError) bool {
	switch err.Kind {
	case WSErrWebSocket:
		if err.Stage == "decode websocket event" {
			return true
		}
		return semanticErrorBody(err.Message)
	case WSErrRequestFailed, WSErrAuthFailed, WSErrNetworkPolicy:
		return true
	case WSErrUnexpectedStatus:
		return nonRetryableStatus(err.Status)
	case WSErrUnexpectedStatusWithBody:
		return nonRetryableStatus(err.Status) || semanticErrorBody(err.Body)
	default:
		return false
	}
}

var retryableMessageNeedles = []string{
	"closed",
	"reset",
	"broken pipe",
	"without closing handshake",
	"connection aborted",
	"connection refused",
	"connection reset",
	"stream ended before first event",
}

// retryableWebSocketMessage mirrors retryable_websocket_message (:679-693).
func retryableWebSocketMessage(message string) bool {
	lower := strings.ToLower(message)
	for _, needle := range retryableMessageNeedles {
		if strings.Contains(lower, needle) {
			return true
		}
	}
	return false
}

var semanticErrorNeedles = []string{
	"previous_response_id",
	"tool schema",
	"schema validation",
	"model",
	"invalid_request",
	"invalid request",
}

// semanticErrorBody mirrors semantic_error_body (:695-707).
func semanticErrorBody(body string) bool {
	lower := strings.ToLower(body)
	for _, needle := range semanticErrorNeedles {
		if strings.Contains(lower, needle) {
			return true
		}
	}
	return false
}

// retryableStatus mirrors retryable_status (:709-711).
func retryableStatus(status int) bool {
	switch status {
	case 500, 502, 503:
		return true
	default:
		return false
	}
}

// nonRetryableStatus mirrors non_retryable_status (:713-715).
func nonRetryableStatus(status int) bool {
	switch status {
	case 400, 401, 403, 422, 429:
		return true
	default:
		return false
	}
}

// wsResult is the internal per-event result a pooled session's actor
// goroutine hands back for one queued response.create, mirroring
// Result<CodexBackendEvent, BackendError> (websocket_transport.rs:127).
type wsResult struct {
	Event backend.Event
	Err   *WSTransportError
}

// wsCommand mirrors WebSocketCommand (:125-128).
type wsCommand struct {
	body   map[string]any
	result chan wsResult
}

// wsSession mirrors CodexWebSocketSession (:118-123). Unlike Rust, where
// dropping the session's mpsc::Sender lets the actor task's recv() observe
// closure, Go's session goroutine holds cmds directly; cancel terminates it
// explicitly via ctx.Done() instead.
type wsSession struct {
	sender  chan *wsCommand
	alive   *atomic.Bool
	done    chan struct{}
	cancel  context.CancelFunc
	chainID backend.ChainID
}

func sessionClosedError() *WSTransportError {
	return &WSTransportError{Kind: WSErrWebSocket, Stage: "queue response.create", Message: "websocket session is closed"}
}

// sendEventStream mirrors CodexWebSocketSession::send_event_stream
// (:363-384).
func (s *wsSession) sendEventStream(body map[string]any) (<-chan wsResult, error) {
	if !s.alive.Load() {
		return nil, sessionClosedError()
	}
	result := make(chan wsResult, wsResultBuffer)
	cmd := &wsCommand{body: body, result: result}
	select {
	case s.sender <- cmd:
		return result, nil
	case <-s.done:
		return nil, sessionClosedError()
	}
}

// Pool is the Go port of WebSocketSessionPool (websocket_transport.rs:108-356
// / 135-356): a per-SessionKey pool of live pooled WebSocket sessions to the
// Codex Responses endpoint, with keepalive, chain-ID tracking, and eviction.
type Pool struct {
	baseURL string
	auth    portauth.Provider
	http    *netpolicy.Client

	mu       sync.Mutex
	sessions map[backend.SessionKey]*wsSession
}

// NewPool builds a Pool. An empty baseURL falls back to DefaultBaseURL.
func NewPool(baseURL string, auth portauth.Provider, httpClient *netpolicy.Client) *Pool {
	if baseURL == "" {
		baseURL = DefaultBaseURL
	}
	return &Pool{
		baseURL:  baseURL,
		auth:     auth,
		http:     httpClient,
		sessions: map[backend.SessionKey]*wsSession{},
	}
}

// HasLiveSession mirrors WebSocketSessionPool::has_live_session (:136-138).
func (p *Pool) HasLiveSession(key backend.SessionKey) bool {
	_, ok := p.LiveChainID(key)
	return ok
}

// LiveChainID mirrors WebSocketSessionPool::live_websocket_chain_id
// (:140-150).
func (p *Pool) LiveChainID(key backend.SessionKey) (backend.ChainID, bool) {
	p.mu.Lock()
	sess, ok := p.sessions[key]
	p.mu.Unlock()
	if !ok || !sess.alive.Load() {
		return "", false
	}
	return sess.chainID, true
}

// EvictSession mirrors WebSocketSessionPool::evict (:258-263).
func (p *Pool) EvictSession(key backend.SessionKey) {
	p.mu.Lock()
	sess, ok := p.sessions[key]
	if ok {
		delete(p.sessions, key)
	}
	p.mu.Unlock()
	if ok {
		sess.cancel()
	}
}

func (p *Pool) getAliveSession(key backend.SessionKey) *wsSession {
	p.mu.Lock()
	sess, ok := p.sessions[key]
	p.mu.Unlock()
	if !ok {
		return nil
	}
	if sess.alive.Load() {
		return sess
	}
	p.EvictSession(key)
	return nil
}

// SendEventStream mirrors WebSocketSessionPool::send_event_stream
// (:152-256): reuse a live session keyed by sessionKey when possible,
// recycling on a retryable failure up to policy.MaxRecycles times, and
// returns a channel of backend.Event plus the WebSocket chain ID used to
// serve the request. Mid-stream failures are delivered as a single
// Type:"error" backend.Event before the channel closes, matching the SSE
// decoder's error convention (sse.go).
func (p *Pool) SendEventStream(ctx context.Context, sessionKey backend.SessionKey, req *backend.Request, policy RetryPolicy) (<-chan backend.Event, backend.ChainID, error) {
	recycles := 0

	for {
		results, chainID, err := p.sendAttempt(ctx, sessionKey, req)
		if err != nil {
			wsErr, _ := err.(*WSTransportError)
			if wsErr != nil && shouldRecycleWebSocketError(wsErr, policy) && recycles < policy.MaxRecycles {
				p.EvictSession(sessionKey)
				recycles++
				continue
			}
			return nil, "", err
		}

		first, ok := <-results
		if !ok {
			werr := &WSTransportError{Kind: WSErrWebSocket, Stage: "read event", Message: "websocket stream ended before first event"}
			if shouldRecycleWebSocketError(werr, policy) && recycles < policy.MaxRecycles {
				p.EvictSession(sessionKey)
				recycles++
				continue
			}
			p.EvictSession(sessionKey)
			return singleErrorEventStream(werr), chainID, nil
		}

		if first.Err != nil {
			if shouldRecycleWebSocketError(first.Err, policy) && recycles < policy.MaxRecycles {
				p.EvictSession(sessionKey)
				recycles++
				continue
			}
			if isTransportLifecycleError(first.Err) {
				p.EvictSession(sessionKey)
			}
			return singleErrorEventStream(first.Err), chainID, nil
		}

		return p.tailEventStream(sessionKey, first.Event, results), chainID, nil
	}
}

// sendAttempt mirrors WebSocketSessionPool::send_attempt (:273-335).
func (p *Pool) sendAttempt(ctx context.Context, key backend.SessionKey, req *backend.Request) (<-chan wsResult, backend.ChainID, error) {
	if sess := p.getAliveSession(key); sess != nil {
		results, err := sess.sendEventStream(buildWebSocketCreateBody(req))
		if err != nil {
			p.EvictSession(key)
			return nil, "", err
		}
		return results, sess.chainID, nil
	}

	if req.PreviousResponseID != nil {
		return nil, "", &WSTransportError{
			Kind:    WSErrWebSocket,
			Stage:   "queue response.create",
			Message: "previous_response_id requires a live websocket session",
		}
	}

	sess, err := p.openSessionWithRefreshRetry(ctx, req)
	if err != nil {
		return nil, "", err
	}
	results, err := sess.sendEventStream(buildWebSocketCreateBody(req))
	if err != nil {
		return nil, "", err
	}

	p.mu.Lock()
	p.sessions[key] = sess
	p.mu.Unlock()

	return results, sess.chainID, nil
}

// tailEventStream forwards first, then continues draining results into a
// backend.Event channel, evicting the pooled session on a lifecycle error
// mid-stream, mirroring the guarded_tail mapping in send_event_stream
// (:212-224).
func (p *Pool) tailEventStream(key backend.SessionKey, first backend.Event, results <-chan wsResult) <-chan backend.Event {
	out := make(chan backend.Event)
	go func() {
		defer close(out)
		out <- first
		if isTerminalBackendEvent(first.Type) {
			return
		}
		for item := range results {
			if item.Err != nil {
				if isTransportLifecycleError(item.Err) {
					p.EvictSession(key)
				}
				errData, _ := json.Marshal(map[string]string{"type": "error", "message": item.Err.Error()})
				out <- backend.Event{Type: "error", Data: errData}
				return
			}
			out <- item.Event
			if isTerminalBackendEvent(item.Event.Type) {
				return
			}
		}
	}()
	return out
}

func singleErrorEventStream(err *WSTransportError) <-chan backend.Event {
	out := make(chan backend.Event, 1)
	errData, _ := json.Marshal(map[string]string{"type": "error", "message": err.Error()})
	out <- backend.Event{Type: "error", Data: errData}
	close(out)
	return out
}

// openSessionWithRefreshRetry mirrors open_session_with_refresh_retry
// (:387-424), simplified to match client.go's requestWithRefreshRetry
// pattern: this codebase's auth.Provider has no
// "is_permanent_refresh_failure" distinction, so (as client.go already
// does for the HTTP path) logout is triggered only when a post-refresh
// retry still comes back 401, not when the refresh call itself fails.
func (p *Pool) openSessionWithRefreshRetry(ctx context.Context, req *backend.Request) (*wsSession, error) {
	sess, err := p.openSession(ctx, req)
	if err == nil {
		return sess, nil
	}
	if !isUnauthorized(err) {
		return nil, err
	}

	if rerr := p.refreshOnUnauthorized(ctx, req); rerr != nil {
		return nil, rerr
	}

	sess, err = p.openSession(ctx, req)
	if err != nil {
		if isUnauthorized(err) {
			_ = p.auth.Logout(ctx, true)
		}
		return nil, err
	}
	return sess, nil
}

func isUnauthorized(err error) bool {
	wsErr, ok := err.(*WSTransportError)
	return ok && wsErr.Kind == WSErrUnexpectedStatus && wsErr.Status == http.StatusUnauthorized
}

func (p *Pool) refreshOnUnauthorized(ctx context.Context, req *backend.Request) error {
	snapshot, err := p.auth.RefreshAndPersist(ctx)
	if err != nil {
		return &WSTransportError{Kind: WSErrAuthFailed, Stage: "refresh auth", Message: err.Error()}
	}

	token, err := p.auth.AccessToken(ctx)
	if err != nil {
		return &WSTransportError{Kind: WSErrAuthFailed, Stage: "reload access token", Message: err.Error()}
	}

	req.AccessToken = token
	req.AccountID = snapshot.AccountID
	return nil
}

// openSession mirrors open_session (:426-443).
func (p *Pool) openSession(ctx context.Context, req *backend.Request) (*wsSession, error) {
	wsURL, err := websocketURL(p.baseURL)
	if err != nil {
		return nil, err
	}
	if perr := p.checkPolicy(wsURL); perr != nil {
		return nil, &WSTransportError{Kind: WSErrNetworkPolicy, Stage: "connect", Message: perr.Error()}
	}

	conn, resp, derr := websocket.Dial(ctx, wsURL, &websocket.DialOptions{
		HTTPClient: p.http.HTTP,
		HTTPHeader: websocketHeaders(req),
	})
	if derr != nil {
		return nil, websocketFailed("connect", resp, derr)
	}

	sessCtx, cancel := context.WithCancel(context.Background())
	alive := &atomic.Bool{}
	alive.Store(true)
	sess := &wsSession{
		sender:  make(chan *wsCommand),
		alive:   alive,
		done:    make(chan struct{}),
		cancel:  cancel,
		chainID: backend.ChainID(uuid.NewString()),
	}
	go runSession(sessCtx, conn, sess.sender, alive, sess.done)
	return sess, nil
}

// checkPolicy enforces netpolicy on the WebSocket URL. netpolicy.Policy only
// accepts http/https schemes (CheckURL), so the ws/wss scheme is normalized
// to http/https for the check only; the actual dial keeps the real ws/wss
// URL.
func (p *Pool) checkPolicy(wsURL string) error {
	u, err := url.Parse(wsURL)
	if err != nil {
		return err
	}
	switch u.Scheme {
	case "wss":
		u.Scheme = "https"
	case "ws":
		u.Scheme = "http"
	}
	return p.http.Policy.CheckURL(u)
}

// websocketURL mirrors websocket_url (:786-809).
func websocketURL(baseURL string) (string, error) {
	parsed, err := url.Parse(baseURL)
	if err != nil {
		return "", websocketMessage("parse url", err)
	}
	switch parsed.Scheme {
	case "https":
		parsed.Scheme = "wss"
	case "http":
		parsed.Scheme = "ws"
	case "wss", "ws":
	default:
		return "", &WSTransportError{
			Kind:    WSErrWebSocket,
			Stage:   "prepare websocket url",
			Message: fmt.Sprintf("unsupported base url scheme: %s", parsed.Scheme),
		}
	}
	parsed.Path = responsesPath
	parsed.RawQuery = ""
	return parsed.String(), nil
}

// websocketHeaders mirrors websocket_request's header construction
// (:445-480).
func websocketHeaders(req *backend.Request) http.Header {
	h := http.Header{}
	h.Set("Authorization", "Bearer "+req.AccessToken.Expose())
	h.Set("chatgpt-account-id", req.AccountID)
	h.Set("OpenAI-Beta", "responses=experimental")
	h.Set("originator", "codex_cli_rs")
	return h
}

// buildWebSocketCreateBody mirrors build_websocket_create_body (:828-844),
// reusing buildRequestBody from client.go.
func buildWebSocketCreateBody(req *backend.Request) map[string]any {
	body := buildRequestBody(req)
	body["type"] = "response.create"
	if req.PreviousResponseID != nil {
		body["previous_response_id"] = *req.PreviousResponseID
	}
	delete(body, "stream")
	return body
}

func isTerminalBackendEvent(eventType string) bool {
	for _, t := range backend.TerminalEvents {
		if t == eventType {
			return true
		}
	}
	return false
}

// decodeWebSocketEvent mirrors websocket_text_to_backend_event (:811-826).
func decodeWebSocketEvent(data []byte) (backend.Event, *WSTransportError) {
	var probe struct {
		Type string `json:"type"`
	}
	if err := json.Unmarshal(data, &probe); err != nil {
		return backend.Event{}, &WSTransportError{Kind: WSErrWebSocket, Stage: "decode websocket event", Message: err.Error()}
	}
	eventType := probe.Type
	if eventType == "" {
		eventType = "message"
	}
	return backend.Event{Type: eventType, Data: json.RawMessage(data)}, nil
}

// runSession is the pooled session's actor goroutine, mirroring
// run_websocket_session (:482-530): it owns the socket exclusively for
// writes, serializes one queued response.create at a time, and sends a
// 20s keepalive ping (KeepaliveInterval) while idle. Unlike Rust, control
// frames (ping/pong/close) are handled inline by coder/websocket's Read
// call itself (see conn.go's handleControl), so no separate idle-item
// branch is needed here.
func runSession(ctx context.Context, conn *websocket.Conn, cmds chan *wsCommand, alive *atomic.Bool, done chan struct{}) {
	defer func() {
		alive.Store(false)
		_ = conn.CloseNow()
		close(done)
	}()

	keepalive := time.NewTicker(KeepaliveInterval)
	defer keepalive.Stop()

	for {
		select {
		case <-ctx.Done():
			return
		case <-keepalive.C:
			pingCtx, cancel := context.WithTimeout(ctx, pingTimeout)
			err := conn.Ping(pingCtx)
			cancel()
			if err != nil {
				return
			}
		case cmd := <-cmds:
			payload, merr := json.Marshal(cmd.body)
			if merr != nil {
				cmd.result <- wsResult{Err: &WSTransportError{Kind: WSErrWebSocket, Stage: "send response.create", Message: merr.Error()}}
				close(cmd.result)
				return
			}
			if werr := conn.Write(ctx, websocket.MessageText, payload); werr != nil {
				cmd.result <- wsResult{Err: websocketMessage("send response.create", werr)}
				close(cmd.result)
				return
			}
			ok := forwardResponseEvents(ctx, conn, cmd.result)
			close(cmd.result)
			if !ok {
				return
			}
		}
	}
}

// forwardResponseEvents mirrors forward_websocket_response_events
// (:532-586): reads frames until a terminal backend event (per
// backend.TerminalEvents) or a read/decode failure. The bool return
// reports whether the session is still usable for a subsequent command.
func forwardResponseEvents(ctx context.Context, conn *websocket.Conn, result chan<- wsResult) bool {
	for {
		typ, data, err := conn.Read(ctx)
		if err != nil {
			result <- wsResult{Err: classifyReadError(err)}
			return false
		}
		if typ != websocket.MessageText && typ != websocket.MessageBinary {
			continue
		}

		event, derr := decodeWebSocketEvent(data)
		if derr != nil {
			result <- wsResult{Err: derr}
			return false
		}

		terminal := isTerminalBackendEvent(event.Type)
		result <- wsResult{Event: event}
		if terminal {
			return true
		}
	}
}
