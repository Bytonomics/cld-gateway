package codexauth

import (
	"context"
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/url"
	"os"
	"os/exec"
	"runtime"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/Bytonomics/cld-gateway/netpolicy"
)

const (
	loginIssuer       = "https://auth.openai.com"
	loginTokenURL     = loginIssuer + "/oauth/token"
	loginAuthorizeURL = loginIssuer + "/oauth/authorize"
	loginClientID     = "app_EMoamEEZ73f0CkXaXp7hrann"
	loginDefaultPort  = 1455
	loginFallbackPort = 1457
	loginTimeout      = 15 * time.Minute
)

var (
	ErrLoginCallbackBindFailed  = errors.New("failed to bind OAuth callback server")
	ErrLoginTimeout             = errors.New("login timed out waiting for browser callback")
	ErrLoginInvalidCallback     = errors.New("OAuth callback missing code or state")
	ErrLoginStateMismatch       = errors.New("OAuth callback state does not match request")
	ErrLoginTokenExchangeFailed = errors.New("OAuth token exchange failed")
)

// LoginOpts configures RunLogin. All fields are optional; Println defaults
// to writing progress lines via fmt.Println.
type LoginOpts struct {
	AuthJSONPath string
	Println      func(string)
}

type pkceCodes struct {
	codeVerifier  string
	codeChallenge string
}

func (o LoginOpts) println(s string) {
	if o.Println != nil {
		o.Println(s)
		return
	}
	fmt.Println(s)
}

// generateLoginState ports login::generate_state.
func generateLoginState() (string, error) {
	buf := make([]byte, 32)
	if _, err := rand.Read(buf); err != nil {
		return "", err
	}
	return base64.RawURLEncoding.EncodeToString(buf), nil
}

// generatePKCE ports login::generate_pkce.
func generatePKCE() (pkceCodes, error) {
	buf := make([]byte, 64)
	if _, err := rand.Read(buf); err != nil {
		return pkceCodes{}, err
	}
	codeVerifier := base64.RawURLEncoding.EncodeToString(buf)
	digest := sha256.Sum256([]byte(codeVerifier))
	codeChallenge := base64.RawURLEncoding.EncodeToString(digest[:])
	return pkceCodes{codeVerifier: codeVerifier, codeChallenge: codeChallenge}, nil
}

// buildAuthorizeURL ports login::build_authorize_url.
func buildAuthorizeURL(redirectURI string, pkce pkceCodes, state string) string {
	q := url.Values{}
	q.Set("response_type", "code")
	q.Set("client_id", loginClientID)
	q.Set("redirect_uri", redirectURI)
	q.Set("scope", "openid profile email offline_access api.connectors.read api.connectors.invoke")
	q.Set("code_challenge", pkce.codeChallenge)
	q.Set("code_challenge_method", "S256")
	q.Set("id_token_add_organizations", "true")
	q.Set("codex_cli_simplified_flow", "true")
	q.Set("state", state)
	q.Set("originator", "gatewayd")
	return loginAuthorizeURL + "?" + q.Encode()
}

// preferredCallbackPort ports login::preferred_callback_port.
func preferredCallbackPort() int {
	if v := os.Getenv("CLD_GATEWAY_AUTH_PORT"); v != "" {
		if p, err := strconv.Atoi(v); err == nil {
			return p
		}
	}
	return loginDefaultPort
}

// openBrowser ports webbrowser::open for the platforms this gateway targets.
// Errors are ignored by the caller, matching the Rust best-effort behavior.
func openBrowser(rawURL string) error {
	switch runtime.GOOS {
	case "darwin":
		return exec.Command("open", rawURL).Start()
	case "windows":
		return exec.Command("rundll32", "url.dll,FileProtocolHandler", rawURL).Start()
	default:
		return exec.Command("xdg-open", rawURL).Start()
	}
}

// newTCPListener is a small indirection over net.Listen for callback-port
// binding, kept separate so its failure path stays a plain error to test.
func newTCPListener(addr string) (net.Listener, error) {
	return net.Listen("tcp", addr)
}

// callbackResult carries the parsed OAuth callback outcome from the
// localhost HTTP handler goroutine to RunLogin.
type callbackResult struct {
	code  string
	state string
	err   error
}

// runCallbackServer ports login's tiny_http-based callback loop
// (crates/gateway-auth-codex/src/login.rs:188-237) onto net/http: it binds
// loginDefaultPort, falling back to loginFallbackPort, serves exactly one
// /auth/callback request (all other paths 404), and returns the parsed
// code/state or an error.
func runCallbackServer(ctx context.Context, opts LoginOpts) (redirectURI string, resultCh <-chan callbackResult, shutdown func(), err error) {
	port := preferredCallbackPort()
	addr := fmt.Sprintf("127.0.0.1:%d", port)

	ln, bindErr := newTCPListener(addr)
	if bindErr != nil {
		port = loginFallbackPort
		addr = fmt.Sprintf("127.0.0.1:%d", port)
		ln, bindErr = newTCPListener(addr)
		if bindErr != nil {
			return "", nil, nil, fmt.Errorf("%w: %v", ErrLoginCallbackBindFailed, bindErr)
		}
		opts.println(fmt.Sprintf("Preferred OAuth callback port %d unavailable; falling back to %d", preferredCallbackPort(), loginFallbackPort))
	} else {
		opts.println(fmt.Sprintf("Using OAuth callback port %d", port))
	}

	redirectURI = fmt.Sprintf("http://localhost:%d/auth/callback", port)

	ch := make(chan callbackResult, 1)
	var once sync.Once

	mux := http.NewServeMux()
	mux.HandleFunc("/auth/callback", func(w http.ResponseWriter, r *http.Request) {
		code := r.URL.Query().Get("code")
		state := r.URL.Query().Get("state")

		if code == "" || state == "" {
			w.WriteHeader(http.StatusBadRequest)
			_, _ = io.WriteString(w, "<html><body><h3>Login failed</h3>Missing code/state.</body></html>")
			once.Do(func() { ch <- callbackResult{err: ErrLoginInvalidCallback} })
			return
		}

		w.WriteHeader(http.StatusOK)
		_, _ = io.WriteString(w, "<html><body><h3>Login complete</h3>You can close this tab.</body></html>")
		once.Do(func() { ch <- callbackResult{code: code, state: state} })
	})
	mux.HandleFunc("/", func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusNotFound)
		_, _ = io.WriteString(w, "<html><body>Not found</body></html>")
	})

	srv := &http.Server{Handler: mux}
	go func() { _ = srv.Serve(ln) }()

	shutdown = func() {
		shutdownCtx, cancel := context.WithTimeout(context.Background(), 2*time.Second)
		defer cancel()
		_ = srv.Shutdown(shutdownCtx)
	}

	return redirectURI, ch, shutdown, nil
}

// exchangeCodeForTokens ports login::exchange_code_for_tokens.
func exchangeCodeForTokens(ctx context.Context, httpClient *netpolicy.Client, redirectURI string, pkce pkceCodes, code string) (idToken, accessToken, refreshToken string, err error) {
	form := url.Values{}
	form.Set("grant_type", "authorization_code")
	form.Set("code", code)
	form.Set("redirect_uri", redirectURI)
	form.Set("client_id", loginClientID)
	form.Set("code_verifier", pkce.codeVerifier)

	req, reqErr := http.NewRequestWithContext(ctx, http.MethodPost, loginTokenURL, strings.NewReader(form.Encode()))
	if reqErr != nil {
		return "", "", "", reqErr
	}
	req.Header.Set("Content-Type", "application/x-www-form-urlencoded")

	res, doErr := httpClient.Do(req)
	if doErr != nil {
		return "", "", "", doErr
	}
	defer func() { _ = res.Body.Close() }()

	bodyBytes, readErr := io.ReadAll(res.Body)
	if readErr != nil {
		return "", "", "", readErr
	}

	if res.StatusCode < 200 || res.StatusCode >= 300 {
		return "", "", "", fmt.Errorf("%w: status %d", ErrLoginTokenExchangeFailed, res.StatusCode)
	}

	var parsed struct {
		IDToken      string `json:"id_token"`
		AccessToken  string `json:"access_token"`
		RefreshToken string `json:"refresh_token"`
	}
	if err := json.Unmarshal(bodyBytes, &parsed); err != nil {
		return "", "", "", fmt.Errorf("%w: %v", ErrLoginTokenExchangeFailed, err)
	}
	return parsed.IDToken, parsed.AccessToken, parsed.RefreshToken, nil
}

// RunLogin ports login_with_chatgpt_and_write_default_auth_json
// (crates/gateway-auth-codex/src/login.rs:175-263): generates state + PKCE,
// binds a localhost callback server (loginDefaultPort, falling back to
// loginFallbackPort), opens the system browser to the authorize URL, waits
// up to loginTimeout for the callback, verifies the returned state, and
// exchanges the code for tokens which are then persisted to auth.json.
func RunLogin(ctx context.Context, opts LoginOpts) error {
	state, err := generateLoginState()
	if err != nil {
		return err
	}
	pkce, err := generatePKCE()
	if err != nil {
		return err
	}

	redirectURI, resultCh, shutdown, err := runCallbackServer(ctx, opts)
	if err != nil {
		return err
	}
	defer shutdown()

	authURL := buildAuthorizeURL(redirectURI, pkce, state)
	opts.println(fmt.Sprintf("\nFinish signing in via your browser\n\nIf the link doesn't open automatically, open the following link to authenticate:\n\n%s\n", authURL))
	_ = openBrowser(authURL)

	var result callbackResult
	select {
	case result = <-resultCh:
	case <-time.After(loginTimeout):
		return ErrLoginTimeout
	case <-ctx.Done():
		return ctx.Err()
	}
	if result.err != nil {
		return result.err
	}
	if result.state != state {
		return ErrLoginStateMismatch
	}

	authJSONPath := opts.AuthJSONPath
	if authJSONPath == "" {
		authJSONPath = DefaultAuthJSONPath()
	}
	store := &Store{path: authJSONPath, http: netpolicy.New(nil)}

	idToken, accessToken, refreshToken, err := exchangeCodeForTokens(ctx, store.httpClient(), redirectURI, pkce, result.code)
	if err != nil {
		return err
	}

	accountID, _ := extractChatGPTAccountIDUnverified(idToken)

	authMode := "chatgpt"
	doc := authJSON{
		AuthMode: &authMode,
		Tokens: &tokens{
			IDToken:      &idToken,
			AccessToken:  &accessToken,
			RefreshToken: &refreshToken,
		},
	}
	if accountID != "" {
		doc.Tokens.AccountID = &accountID
	}

	return store.atomicWriteJSON(&doc)
}
