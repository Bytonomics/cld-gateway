// Package codex implements core/domain/port/backend.Backend against the
// Codex/ChatGPT backend HTTP API, matching crates/gateway-backend-codex.
package codex

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"strings"
	"time"

	portauth "github.com/Bytonomics/cld-gateway/core/domain/port/auth"
	"github.com/Bytonomics/cld-gateway/core/domain/port/backend"
	"github.com/Bytonomics/cld-gateway/core/impl/port/auth/codexauth"
	"github.com/Bytonomics/cld-gateway/netpolicy"
)

const (
	// DefaultBaseURL mirrors CodexBackendClient::default (client.rs:59-68).
	DefaultBaseURL = "https://chatgpt.com"

	// DefaultUnaryTimeout is a G4 (approved) deviation from the Rust client,
	// which has request_timeout: None (client.rs:64). Streaming requests are
	// unaffected: only SendUnary applies this timeout.
	DefaultUnaryTimeout = 120 * time.Second

	// DefaultStreamIdleReadTimeout is a G4 (approved) idle-read timeout for
	// streaming responses. Matches StreamWriter's DefaultIdleEventTimeout for
	// symmetry: if no SSE line arrives within this window, the decoder closes
	// the body and emits an error event to prevent goroutine leaks.
	DefaultStreamIdleReadTimeout = 60 * time.Second

	responsesPath = "/backend-api/codex/responses"

	// storeResponsesForContinuation mirrors
	// crate::types::STORE_RESPONSES_FOR_CONTINUATION (types.rs:7): the
	// gateway never asks Codex to persist responses server-side.
	storeResponsesForContinuation = false

	// maxErrorBodyBytes mirrors truncate_error_body's MAX (client.rs:369).
	maxErrorBodyBytes = 8 * 1024
)

// Config configures a Client.
type Config struct {
	BaseURL               string
	UnaryTimeout          time.Duration
	StreamIdleReadTimeout time.Duration
}

// Client implements backend.Backend against the Codex "Responses-like"
// backend endpoint, matching CodexBackendClient (client.rs:51-322).
type Client struct {
	cfg  Config
	auth portauth.Provider
	http *netpolicy.Client
}

var _ backend.Backend = (*Client)(nil)

// New builds a Client. Zero-value Config fields fall back to
// DefaultBaseURL / DefaultUnaryTimeout / DefaultStreamIdleReadTimeout.
func New(cfg Config, auth portauth.Provider, httpClient *netpolicy.Client) *Client {
	if cfg.BaseURL == "" {
		cfg.BaseURL = DefaultBaseURL
	}
	if cfg.UnaryTimeout == 0 {
		cfg.UnaryTimeout = DefaultUnaryTimeout
	}
	if cfg.StreamIdleReadTimeout == 0 {
		cfg.StreamIdleReadTimeout = DefaultStreamIdleReadTimeout
	}
	return &Client{cfg: cfg, auth: auth, http: httpClient}
}

// StatusError is returned when the Codex backend responds with a non-success
// HTTP status, mirroring BackendError::UnexpectedStatus /
// UnexpectedStatusWithBody (client.rs:45-48).
type StatusError struct {
	Status int
	Body   string
}

func (e *StatusError) Error() string {
	if e.Body != "" {
		return fmt.Sprintf("unexpected response status %d: %s", e.Status, e.Body)
	}
	return fmt.Sprintf("unexpected response status %d", e.Status)
}

// UpstreamStatus implements core/domain/port/backend.UpstreamStatusError.
func (e *StatusError) UpstreamStatus() int {
	return e.Status
}

// UpstreamBody implements core/domain/port/backend.UpstreamStatusError.
func (e *StatusError) UpstreamBody() string {
	return e.Body
}

func newStatusError(status int, rawBody []byte) error {
	trimmed := strings.TrimSpace(string(rawBody))
	if trimmed == "" {
		return &StatusError{Status: status}
	}
	return &StatusError{Status: status, Body: truncateErrorBody(rawBody)}
}

// truncateErrorBody mirrors client.rs:367-376.
func truncateErrorBody(body []byte) string {
	if len(body) <= maxErrorBodyBytes {
		return string(body)
	}
	return string(body[:maxErrorBodyBytes]) + "…(truncated)"
}

func httpStatus(err error) int {
	var se *StatusError
	if errors.As(err, &se) {
		return se.Status
	}
	return 0
}

// SendUnary sends req and returns the full response body, mirroring
// CodexBackendClient::send (client.rs:113-128) composed with
// send_with_refresh_retry's 401 handling (client.rs:191-229).
func (c *Client) SendUnary(ctx context.Context, req *backend.Request) (*backend.Response, error) {
	if c.cfg.UnaryTimeout > 0 {
		var cancel context.CancelFunc
		ctx, cancel = context.WithTimeout(ctx, c.cfg.UnaryTimeout)
		defer cancel()
	}

	res, err := c.requestWithRefreshRetry(ctx, req)
	if err != nil {
		return nil, err
	}
	defer func() { _ = res.Body.Close() }()

	bodyBytes, err := io.ReadAll(res.Body)
	if err != nil {
		return nil, fmt.Errorf("codex backend: read response body: %w", err)
	}

	return &backend.Response{Status: uint16(res.StatusCode), Body: string(bodyBytes)}, nil
}

// SendStream sends req with Accept: text/event-stream and decodes the
// response body into a channel of backend.Event, mirroring
// CodexBackendClient::send_streaming (client.rs:137-184) composed with
// send_streaming_with_refresh_retry (client.rs:236-273) and
// response_to_event_stream (client.rs:276-291).
func (c *Client) SendStream(ctx context.Context, req *backend.Request) (<-chan backend.Event, error) {
	res, err := c.requestWithRefreshRetry(ctx, req)
	if err != nil {
		return nil, err
	}
	return DecodeEventStream(ctx, res.Body, c.cfg.StreamIdleReadTimeout), nil
}

// requestWithRefreshRetry ports the shared "refresh once, retry once" 401
// contract from send_with_refresh_retry / send_streaming_with_refresh_retry
// (client.rs:191-229, 236-273). If the retried request also comes back 401,
// that is treated as a permanent auth failure and triggers logout (this
// task's decision override: the Rust source only logs out when the refresh
// call itself permanently fails, not when a post-refresh retry still 401s).
func (c *Client) requestWithRefreshRetry(ctx context.Context, req *backend.Request) (*http.Response, error) {
	res, err := c.doRequest(ctx, req)
	if err == nil {
		return res, nil
	}
	if httpStatus(err) != http.StatusUnauthorized {
		return nil, err
	}

	if rerr := c.refreshOnUnauthorized(ctx, req); rerr != nil {
		if errors.Is(rerr, codexauth.ErrRefreshUnauthorized) {
			_ = c.auth.Logout(ctx, true)
		}
		return nil, rerr
	}

	res, err = c.doRequest(ctx, req)
	if err != nil {
		if httpStatus(err) == http.StatusUnauthorized {
			_ = c.auth.Logout(ctx, true)
		}
		return nil, err
	}
	return res, nil
}

// refreshOnUnauthorized ports the refresh branch of send_with_refresh_retry
// (client.rs:202-223): refresh via the auth provider, reload the access
// token, and update req in place (account_id from the refresh snapshot).
func (c *Client) refreshOnUnauthorized(ctx context.Context, req *backend.Request) error {
	snapshot, err := c.auth.RefreshAndPersist(ctx)
	if err != nil {
		return fmt.Errorf("codex backend: refresh auth: %w", err)
	}

	token, err := c.auth.AccessToken(ctx)
	if err != nil {
		return fmt.Errorf("codex backend: reload access token: %w", err)
	}

	req.AccessToken = token
	req.AccountID = snapshot.AccountID
	return nil
}

// doRequest builds and sends the HTTP request, mirroring
// CodexBackendClient::send_streaming's request construction and status
// handling (client.rs:137-184). On success the caller owns res.Body and must
// close it.
func (c *Client) doRequest(ctx context.Context, req *backend.Request) (*http.Response, error) {
	endpoint := strings.TrimRight(c.cfg.BaseURL, "/") + responsesPath

	bodyBytes, err := json.Marshal(buildRequestBody(req))
	if err != nil {
		return nil, fmt.Errorf("codex backend: marshal request body: %w", err)
	}

	httpReq, err := http.NewRequestWithContext(ctx, http.MethodPost, endpoint, bytes.NewReader(bodyBytes))
	if err != nil {
		return nil, fmt.Errorf("codex backend: build request: %w", err)
	}
	httpReq.Header.Set("Content-Type", "application/json")
	httpReq.Header.Set("Authorization", "Bearer "+req.AccessToken.Expose())
	httpReq.Header.Set("chatgpt-account-id", req.AccountID)
	httpReq.Header.Set("OpenAI-Beta", "responses=experimental")
	httpReq.Header.Set("originator", "codex_cli_rs")
	httpReq.Header.Set("Accept", "text/event-stream")

	res, err := c.http.Do(httpReq)
	if err != nil {
		return nil, fmt.Errorf("codex backend: send request: %w", err)
	}

	if res.StatusCode >= 300 {
		defer func() { _ = res.Body.Close() }()
		raw, _ := io.ReadAll(res.Body)
		return nil, newStatusError(res.StatusCode, raw)
	}

	slog.Info("codex backend: response headers",
		"status", res.StatusCode,
		"content_type", res.Header.Get("Content-Type"),
		"content_length", res.Header.Get("Content-Length"),
		"transfer_encoding", strings.Join(res.TransferEncoding, ","))

	return res, nil
}

// buildRequestBody mirrors build_request_body (client.rs:378-445), including
// the OpenAI strict-schema gate (crate::schema_gate::apply_openai_strict_schema_gate)
// called as the final step before returning. previous_response_id is intentionally
// not sent here, matching build_request_body, which never includes it in the HTTP
// body; incremental transport reuse is a pooled-WebSocket concern (wspool.go).
func buildRequestBody(req *backend.Request) map[string]any {
	body := map[string]any{
		"model":               req.Model,
		"instructions":        req.Instructions,
		"input":               nonNilObjects(req.Input),
		"tools":               nonNilObjects(req.Tools),
		"tool_choice":         req.ToolChoice,
		"parallel_tool_calls": req.ParallelToolCalls,
		"store":               storeResponsesForContinuation,
		"stream":              req.Stream,
		"include":             nonNilStrings(req.Include),
	}
	if req.Reasoning != nil {
		body["reasoning"] = *req.Reasoning
	}
	if req.Text != nil {
		body["text"] = *req.Text
	}
	if req.ServiceTier != nil {
		body["service_tier"] = *req.ServiceTier
	}
	if req.ClientMetadata != nil {
		body["client_metadata"] = req.ClientMetadata
	}
	ApplyOpenAIStrictSchemaGate(body)
	return body
}

func nonNilObjects(v []map[string]any) []map[string]any {
	if v == nil {
		return []map[string]any{}
	}
	return v
}

func nonNilStrings(v []string) []string {
	if v == nil {
		return []string{}
	}
	return v
}

// Capabilities reports what this Client can do today: HTTP SSE only. The
// pooled-WebSocket transport (wspool.go, a separate FILEMAP file/wave) is
// what will make WebSocketDelta true; until it lands, EvictSession /
// HasLiveSession / LiveChainID below are no-ops/zero-values rather than
// reporting a capability this Client cannot deliver.
func (c *Client) Capabilities() backend.Capabilities {
	return backend.Capabilities{WebSocketDelta: false, ServerSideState: false}
}

// EvictSession is a no-op until wspool.go's session pool is wired in.
func (c *Client) EvictSession(key backend.SessionKey) {}

// HasLiveSession always reports false until wspool.go's session pool is
// wired in.
func (c *Client) HasLiveSession(key backend.SessionKey) bool { return false }

// LiveChainID always reports (\"\", false) until wspool.go's session pool is
// wired in.
func (c *Client) LiveChainID(key backend.SessionKey) (backend.ChainID, bool) {
	return "", false
}

// codexUsagePath is the Codex backend's live usage/rate-limit endpoint,
// mirroring fetch_live_usage_data (translate_executor.rs:150-183).
const codexUsagePath = "/api/codex/usage"

// FetchStatusData implements backend.Backend's FetchStatusData for the
// Codex backend: a live GET against codexUsagePath using this Client's own
// auth/http wiring, then normalizes Codex's own response shape into the
// backend-agnostic keys the translated "status" command executor
// (core/impl/services/translate_executor.go) expects: plan_type,
// rate_limits, spend_control, usage_raw. All Codex-specific field-name
// knowledge (rate_limit/primary_window/individual_limit, etc.) stays in
// this package - the executor never needs to know which backend answered.
func (c *Client) FetchStatusData(ctx context.Context) (map[string]any, error) {
	token, err := c.auth.AccessToken(ctx)
	if err != nil {
		return nil, fmt.Errorf("codex backend: load access token: %w", err)
	}
	accountID, err := c.auth.AccountID(ctx)
	if err != nil {
		return nil, fmt.Errorf("codex backend: load account id: %w", err)
	}

	url := strings.TrimRight(c.cfg.BaseURL, "/") + codexUsagePath
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, url, nil)
	if err != nil {
		return nil, fmt.Errorf("codex backend: build usage request: %w", err)
	}
	req.Header.Set("Authorization", "Bearer "+token.Expose())
	req.Header.Set("chatgpt-account-id", accountID)

	res, err := c.http.Do(req)
	if err != nil {
		return nil, fmt.Errorf("codex backend: send usage request: %w", err)
	}
	defer func() { _ = res.Body.Close() }()

	body, err := io.ReadAll(res.Body)
	if err != nil {
		return nil, fmt.Errorf("codex backend: read usage response: %w", err)
	}
	if res.StatusCode < 200 || res.StatusCode >= 300 {
		return nil, newStatusError(res.StatusCode, body)
	}

	var usage map[string]any
	if err := json.Unmarshal(body, &usage); err != nil {
		return nil, fmt.Errorf("codex backend: parse usage response: %w", err)
	}

	normalized := map[string]any{
		"rate_limits": normalizeRateLimits(usage),
		"usage_raw":   usage,
	}
	if planType, ok := usage["plan_type"]; ok {
		normalized["plan_type"] = planType
	}
	if spend, ok := usage["spend_control"].(map[string]any); ok {
		normalized["spend_control"] = normalizeSpendControl(spend)
	}
	return normalized, nil
}

// normalizeRateLimits normalizes Codex's rate-limit response shape into
// stable, backend-agnostic fields, mirroring normalize_rate_limits
// (translate_executor.rs:186-237).
func normalizeRateLimits(usage map[string]any) map[string]any {
	limits := map[string]any{}

	if rl, ok := usage["rate_limit"].(map[string]any); ok {
		primary := map[string]any{
			"allowed":       rl["allowed"],
			"limit_reached": rl["limit_reached"],
		}
		if pw, ok := rl["primary_window"].(map[string]any); ok {
			primary["used_percent"] = pw["used_percent"]
			primary["reset_at"] = pw["reset_at"]
			primary["window_seconds"] = pw["limit_window_seconds"]
		}
		limits["primary"] = primary

		if sw, ok := rl["secondary_window"].(map[string]any); ok {
			limits["secondary"] = map[string]any{
				"used_percent":   sw["used_percent"],
				"reset_at":       sw["reset_at"],
				"window_seconds": sw["limit_window_seconds"],
			}
		}
	}

	if additional, ok := usage["additional_rate_limits"].([]any); ok {
		entries := make([]any, 0, len(additional))
		for _, raw := range additional {
			entry, ok := raw.(map[string]any)
			if !ok {
				continue
			}
			name, ok := entry["limit_name"].(string)
			if !ok {
				continue
			}
			rl, ok := entry["rate_limit"].(map[string]any)
			if !ok {
				continue
			}
			pw, ok := rl["primary_window"].(map[string]any)
			if !ok {
				continue
			}
			entries = append(entries, map[string]any{
				"limit_name":     name,
				"allowed":        rl["allowed"],
				"limit_reached":  rl["limit_reached"],
				"used_percent":   pw["used_percent"],
				"reset_at":       pw["reset_at"],
				"window_seconds": pw["limit_window_seconds"],
			})
		}
		if len(entries) > 0 {
			limits["additional"] = entries
		}
	}

	return limits
}

// normalizeSpendControl normalizes Codex's spend-control response shape,
// mirroring normalize_spend_control (translate_executor.rs:240-256).
func normalizeSpendControl(spend map[string]any) map[string]any {
	result := map[string]any{
		"reached": spend["reached"],
	}
	if limit, ok := spend["individual_limit"].(map[string]any); ok {
		result["individual_limit"] = map[string]any{
			"source":            limit["source"],
			"limit":             limit["limit"],
			"used":              limit["used"],
			"remaining":         limit["remaining"],
			"used_percent":      limit["used_percent"],
			"remaining_percent": limit["remaining_percent"],
			"reset_at":          limit["reset_at"],
		}
	}
	return result
}
