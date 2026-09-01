// Command cld-gateway is the runnable daemon entrypoint. It is a thin CLI
// shell: argv `serve` (default) or `login [openai|gemini]`; all real work is
// delegated to config/, core/impl/port/auth/codexauth, tui/, and app/.
// Ported from crates/gatewayd/src/main.rs.
package main

import (
	"bufio"
	"context"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"os"
	"strings"

	"github.com/Bytonomics/cld-gateway/app"
	"github.com/Bytonomics/cld-gateway/config"
	"github.com/Bytonomics/cld-gateway/core/impl/port/auth/codexauth"
	"github.com/Bytonomics/cld-gateway/tui"
)

func main() {
	if err := Main(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

// Vendor identifies which backend a login/preflight flow targets, mirroring
// crates/gatewayd/src/main.rs Vendor (:24-27).
type Vendor int

const (
	VendorOpenAI Vendor = iota
	VendorGemini
)

type cliCommand int

const (
	cmdServe cliCommand = iota
	cmdLogin
)

// Main parses argv and runs the resulting command: `serve` (default) or
// `login [openai|gemini]`, mirroring main/parse_command_from_args
// (main.rs:29-87).
func Main() error {
	cmd, vendor, err := parseCommand(os.Args)
	if err != nil {
		return err
	}

	switch cmd {
	case cmdServe:
		return runServe(context.Background())
	case cmdLogin:
		return runLogin(context.Background(), vendor)
	default:
		return fmt.Errorf("unknown command")
	}
}

// parseCommand ports parse_command_from_args (main.rs:53-87).
func parseCommand(args []string) (cliCommand, Vendor, error) {
	switch len(args) {
	case 1:
		return cmdServe, VendorOpenAI, nil
	case 2:
		switch args[1] {
		case "serve":
			return cmdServe, VendorOpenAI, nil
		case "login":
			return cmdLogin, VendorOpenAI, nil
		default:
			return 0, 0, fmt.Errorf("unknown command %q; expected 'serve' or 'login [vendor]'", args[1])
		}
	case 3:
		switch args[1] {
		case "login":
			switch args[2] {
			case "openai":
				return cmdLogin, VendorOpenAI, nil
			case "gemini":
				return cmdLogin, VendorGemini, nil
			default:
				return 0, 0, fmt.Errorf("unknown vendor %q; expected 'openai' or 'gemini'", args[2])
			}
		case "serve":
			return 0, 0, errors.New("too many arguments; expected 'serve' or 'login [vendor]'")
		default:
			return 0, 0, fmt.Errorf("unknown command %q; expected 'serve' or 'login [vendor]'", args[1])
		}
	default:
		return 0, 0, errors.New("too many arguments; expected 'serve' or 'login [vendor]'")
	}
}

// runServe wires the auth preflight, config load, and app server startup,
// mirroring run_serve (main.rs:89-113).
func runServe(ctx context.Context) error {
	// ✱G3 (open gap, not yet owner-approved): graceful shutdown
	// (signal.NotifyContext + Shutdown drain) is intentionally NOT wired
	// here. Ported as-is from Rust's non-graceful axum::serve(...).await.
	if err := authPreflightForServe(ctx, VendorOpenAI); err != nil {
		return fmt.Errorf("auth preflight failed: %w", err)
	}

	configPath := config.DefaultPath()
	slog.Info("using gateway config file", "config_path", configPath)

	cfg, err := config.Load(configPath)
	if err != nil {
		return fmt.Errorf("load gateway config: %w", err)
	}

	providers, err := app.Initialize(cfg)
	if err != nil {
		return fmt.Errorf("initialize app: %w", err)
	}

	e := app.NewEcho(providers)

	slog.Info("listening", "addr", cfg.Network.ListenAddr)
	return app.RunServer(e, cfg.Network.ListenAddr)
}

// authPreflightForServe runs the non-interactive OpenAI auth preflight
// before serve startup, mirroring auth_preflight_for_serve (main.rs:115-123).
func authPreflightForServe(ctx context.Context, vendor Vendor) error {
	switch vendor {
	case VendorOpenAI:
		return authPreflightOpenAI(ctx)
	case VendorGemini:
		// Gemini login is accepted by the CLI but has no configured
		// serve-mode runtime yet; skip the preflight entirely
		// (main.rs:118-121).
		slog.Info("Gemini auth is not yet configured for serve mode; skipping preflight")
		return nil
	}
	return nil
}

// authPreflightOpenAI implements the full 4-case preflight matrix from
// auth_preflight_openai (main.rs:125-167):
//
//	(a) present+ready   -> refresh + health-check; login only on failure
//	(b) present+incomplete -> login
//	(c) absent          -> login
//	(d) load-error       -> login
//
// codexauth.Store.Status already folds (b)/(c) together (a missing
// auth.json normalizes to a zero-value, non-logged-in Status rather than an
// error), so both are handled by the single "!status.IsLoggedIn" branch
// below.
//
// Auth failures abort startup: if login fails at any point, the error
// propagates up through authPreflightForServe to runServe, terminating the
// process. This matches the Rust behavior (main.rs:125-167), where all four
// preflight cases use `?` to propagate login errors up through run_serve.
func authPreflightOpenAI(ctx context.Context) error {
	store := codexauth.NewDefault()

	status, err := store.Status(ctx)
	if err != nil {
		slog.Warn("gateway auth check failed; launching login flow", "error", err)
		return preflightLoginChatGPT(ctx)
	}

	if !status.IsLoggedIn {
		slog.Warn("gateway auth not found or incomplete; launching login flow")
		return preflightLoginChatGPT(ctx)
	}

	slog.Info("gateway auth present; validating ChatGPT auth", "account_id", status.AccountID)
	snapshot, err := store.RefreshAndPersist(ctx)
	if err != nil {
		slog.Warn("gateway auth health check failed; re-running login flow", "error", err)
		return preflightLoginChatGPT(ctx)
	}
	slog.Info("gateway auth health check succeeded", "account_id", snapshot.AccountID)
	return nil
}

func preflightLoginChatGPT(ctx context.Context) error {
	if err := codexauth.RunLogin(ctx, codexauth.LoginOpts{}); err != nil {
		slog.Warn("gateway login failed during serve preflight", "error", err)
		return err
	}
	slog.Info("gateway login completed during serve preflight")
	return nil
}

// runLogin dispatches to the vendor-specific interactive login flow,
// mirroring login::run_login (login/mod.rs:9-14).
func runLogin(ctx context.Context, vendor Vendor) error {
	switch vendor {
	case VendorOpenAI:
		return runLoginOpenAI(ctx)
	case VendorGemini:
		// Mirrors login/gemini.rs login (:4-6): accepted by the CLI, but
		// not yet supported.
		return errors.New("gemini login is not yet supported, use 'cld-gateway login openai' for now")
	default:
		return fmt.Errorf("unknown vendor")
	}
}

// runLoginOpenAI shows the ChatGPT-OAuth-vs-API-key picker and runs the
// selected flow, mirroring login/openai.rs login (:13-19).
func runLoginOpenAI(ctx context.Context) error {
	selection, err := tui.RunLoginSelector()
	if err != nil {
		return err
	}

	switch selection {
	case tui.VendorChatGPT:
		return runLoginChatGPT(ctx)
	case tui.VendorAPIKey:
		return runLoginAPIKey(ctx)
	default:
		return fmt.Errorf("unknown login selection")
	}
}

// runLoginChatGPT signs in via ChatGPT OAuth (browser-based), mirroring
// login/openai.rs login_with_chatgpt (:26-38).
func runLoginChatGPT(ctx context.Context) error {
	if err := codexauth.RunLogin(ctx, codexauth.LoginOpts{}); err != nil {
		fmt.Fprintf(os.Stderr, "\nLogin failed: %v\n\n", err)
		return err
	}
	fmt.Print("\nLogin successful.\n\n")
	return nil
}

// runLoginAPIKey signs in with an OpenAI API key via an interactive prompt,
// mirroring login/openai.rs login_with_api_key / prompt_api_key (:41-63).
func runLoginAPIKey(ctx context.Context) error {
	apiKey, err := promptAPIKey()
	if err != nil {
		return err
	}

	store := codexauth.NewDefault()
	if err := store.WriteOpenAIAPIKey(ctx, apiKey); err != nil {
		return err
	}
	fmt.Print("\nAPI key saved.\n\n")
	return nil
}

// promptAPIKey ports prompt_api_key (login/openai.rs:49-63).
func promptAPIKey() (string, error) {
	fmt.Print("\nPaste your OpenAI API key. This stores your key for OpenAI-backed auth flows; /v1/models now reads ~/.claude_gateway/settings.json.\n\n")
	fmt.Print("OPENAI_API_KEY: ")

	reader := bufio.NewReader(os.Stdin)
	line, err := reader.ReadString('\n')
	if err != nil && !errors.Is(err, io.EOF) {
		return "", err
	}
	key := strings.TrimSpace(line)
	if key == "" {
		return "", errors.New("empty API key")
	}
	return key, nil
}
