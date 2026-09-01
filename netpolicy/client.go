// Package netpolicy is the Go port of crates/gateway-net: it centralizes
// outbound network policy for the gateway. Every outbound HTTP call must go
// through a *Client so the allow/deny policy applies uniformly, including on
// every redirect hop.
package netpolicy

import (
	"errors"
	"fmt"
	"net/http"
	"net/url"
	"os"
	"strings"
)

// DefaultAllowedHosts mirrors crates/gateway-net/src/lib.rs:7.
var DefaultAllowedHosts = []string{"api.openai.com", "auth.openai.com", "chatgpt.com"}

// DeniedHostSuffixes mirrors crates/gateway-net/src/lib.rs:8. These hosts are
// blocked unconditionally, even if a caller configures them as allowed
// (ARCHITECTURE_v2 invariant #1).
var DeniedHostSuffixes = []string{"anthropic.com", "claude.ai"}

var (
	ErrInvalidURL       = errors.New("invalid outbound URL")
	ErrAnthropicBlocked = errors.New("outbound network call blocked: Anthropic endpoints are forbidden")
	ErrHostNotAllowed   = errors.New("outbound network call blocked: host is not in gateway allowlist")
	ErrSchemeNotAllowed = errors.New("outbound network call blocked: scheme is not allowed")
)

// Policy is the allow/deny decision function, ported from
// GatewayNetworkPolicy (crates/gateway-net/src/lib.rs:22-107).
type Policy struct {
	allowedHosts map[string]struct{}
}

// NewPolicy builds a Policy from the given allowed hosts, plus the built-in
// defaults. GATEWAY_ALLOWED_OUTBOUND_HOSTS (comma-separated) is merged in as
// well, matching the Rust Default impl (lib.rs:27-35). Denied hosts are
// silently dropped even if passed in explicitly (lib.rs:58).
func NewPolicy(hosts []string) *Policy {
	p := &Policy{allowedHosts: map[string]struct{}{}}
	p.extendAllowedHosts(DefaultAllowedHosts)
	if raw := os.Getenv("GATEWAY_ALLOWED_OUTBOUND_HOSTS"); raw != "" {
		p.extendAllowedHosts(strings.Split(raw, ","))
	}
	p.extendAllowedHosts(hosts)
	return p
}

func (p *Policy) extendAllowedHosts(hosts []string) {
	for _, h := range hosts {
		normalized := normalizeHost(h)
		if normalized == "" || isDeniedHost(normalized) {
			continue
		}
		p.allowedHosts[normalized] = struct{}{}
	}
}

// CheckURL applies the policy to u: scheme must be http/https, the host must
// not be an Anthropic/Claude host, and must either be localhost or in the
// allowlist.
func (p *Policy) CheckURL(u *url.URL) error {
	scheme := u.Scheme
	if scheme != "http" && scheme != "https" {
		return fmt.Errorf("%w: scheme %q", ErrSchemeNotAllowed, scheme)
	}

	hostname := u.Hostname()
	if hostname == "" {
		return fmt.Errorf("%w: %s: missing host", ErrInvalidURL, u.String())
	}
	host := normalizeHost(hostname)

	if isDeniedHost(host) {
		return fmt.Errorf("%w: %s", ErrAnthropicBlocked, host)
	}

	if isLocalhost(host) {
		return nil
	}
	if _, ok := p.allowedHosts[host]; ok {
		return nil
	}

	return fmt.Errorf("%w: %s", ErrHostNotAllowed, host)
}

// CheckURLString parses and checks a raw URL string.
func (p *Policy) CheckURLString(raw string) error {
	u, err := url.Parse(raw)
	if err != nil {
		return fmt.Errorf("%w: %s: %v", ErrInvalidURL, raw, err)
	}
	return p.CheckURL(u)
}

func normalizeHost(host string) string {
	host = strings.TrimSpace(host)
	host = strings.Trim(host, "[]")
	host = strings.TrimSuffix(host, ".")
	return strings.ToLower(host)
}

func isDeniedHost(host string) bool {
	for _, denied := range DeniedHostSuffixes {
		if host == denied || strings.HasSuffix(host, "."+denied) {
			return true
		}
	}
	return false
}

func isLocalhost(host string) bool {
	switch host {
	case "localhost", "127.0.0.1", "::1", "0.0.0.0":
		return true
	default:
		return false
	}
}

// Client is the Go port of GatewayHttpClient (lib.rs:109-166): an *http.Client
// wrapper that enforces Policy on the initial request and on every redirect
// hop via CheckRedirect.
type Client struct {
	HTTP   *http.Client
	Policy *Policy
}

// New builds a Client whose Policy allows allowedHosts (plus the built-in
// defaults) and denies Anthropic/Claude hosts unconditionally.
func New(allowedHosts []string) *Client {
	policy := NewPolicy(allowedHosts)
	c := &Client{Policy: policy}
	c.HTTP = &http.Client{
		CheckRedirect: func(req *http.Request, via []*http.Request) error {
			if err := policy.CheckURL(req.URL); err != nil {
				return err
			}
			if len(via) >= 10 {
				return errors.New("stopped after 10 redirects")
			}
			return nil
		},
	}
	return c
}

// Do checks req.URL against the policy before sending it, then delegates to
// the wrapped http.Client (which re-checks the policy on every redirect hop
// via CheckRedirect).
func (c *Client) Do(req *http.Request) (*http.Response, error) {
	if err := c.Policy.CheckURL(req.URL); err != nil {
		return nil, err
	}
	return c.HTTP.Do(req)
}
