# cld-gateway

An Anthropic-compatible HTTP proxy that routes requests through the ChatGPT/Codex backend.

---

## Installation

### Homebrew tap

```sh
brew tap bytonomics/tap
brew install cld-gateway
```

> **Note:** Homebrew availability depends on the separate `bytonomics/homebrew-tap` repo being updated for the release you want to install.

### Shell installer

```sh
curl -fsSL https://github.com/bytonomics/gateway/releases/latest/download/install.sh | sh
```

Or with version pinning:

```sh
curl -fsSL https://github.com/bytonomics/gateway/releases/latest/download/install.sh | sh -s -- --release 0.1.0
```

### Direct download

Download pre-built binaries from the [GitHub Releases page](https://github.com/bytonomics/gateway/releases).

Verify checksums using `cld-gateway-package_SHA256SUMS`, which is published alongside every release.

---

## Quick start

### 1. Authenticate (one-time setup)

```sh
cld-gateway login openai
```

This opens an interactive login menu where you can choose:
- **Sign in with ChatGPT** - OAuth authentication via browser
- **Provide API key** - Paste an OpenAI API key directly

Credentials are saved to `~/.gateway/auth.json` for reuse on subsequent runs.

### 2. Start the daemon

```sh
cld-gateway serve
```

Or simply:

```sh
cld-gateway
```

The daemon starts a non-interactive HTTP server on `127.0.0.1:8080` and uses the credentials from your previous login.

---

## Commands

| Command | Description |
|---|---|
| `cld-gateway` or `cld-gateway serve` | Start the daemon (non-interactive) |
| `cld-gateway login` | Interactive login to OpenAI (ChatGPT OAuth or API key) |
| `cld-gateway login openai` | Same as `cld-gateway login` (defaults to OpenAI) |

---

## Authentication workflow

The `cld-gateway login` command is a foreground, interactive process:

1. **Run login:** `cld-gateway login openai`
2. **Select method:** A TUI menu appears with options:
   - "Sign in with ChatGPT" (opens browser for OAuth)
   - "Provide API key" (paste your OpenAI API key)
3. **Credentials saved:** Auth is written to `~/.gateway/auth.json`
4. **Start daemon:** Run `cld-gateway serve` to start the background HTTP server
5. **Requests are routed:** The daemon routes requests through the ChatGPT backend using your saved credentials

**Token refresh:** If a token expires, the daemon automatically refreshes it on the next upstream request — no manual intervention needed.

---

## Daemon behavior (non-interactive)

The daemon (`cld-gateway serve` or `cld-gateway`) is completely non-interactive:

- **No startup prompts:** Daemon startup does not open browsers, prompt for credentials, or show login menus
- **Uses persisted auth:** Daemon reads credentials from `~/.gateway/auth.json` (written by the login command)
- **Auto-refresh:** If a token is expired or near-expiration, the daemon automatically refreshes it on the next request
- **No blocking:** If auth is missing or invalid:
  - Daemon continues to run (does not exit or block)
  - HTTP responses include a remediation message telling the client to run: `cld-gateway login openai`
  - Tools like Claude Code can surface this message to guide the user

---

## Environment variables

| Variable | Default | Description |
|---|---|---|
| `CLD_GATEWAY_LISTEN_ADDR` | `127.0.0.1:8080` | Listen address and port |
| `GATEWAY_HOME` | `~/.gateway` | Override all default `~/.gateway` paths at once |
| `GATEWAY_AUTH_JSON_PATH` | `~/.gateway/auth.json` | Auth credentials file path |
| `CLD_GATEWAY_AUTH_PORT` | `1455` | OAuth callback port (see Authentication section) |
| `CLD_GATEWAY_LOG_PATH` | `~/.gateway/logs/http-exchange.jsonl` | Exchange log file path |
| `CLD_GATEWAY_STATE_DB_PATH` | `~/.gateway/state/tool_calls.sqlite` | Tool-call state DB path |
| `GATEWAY_CONFIG_PATH` | `~/.gateway/config.json` | Gateway config file path |

---

## Side-by-side dev and release

You can run a dev build and a release build simultaneously by pointing each at different ports and data directories:

```sh
# Release build — login first
cld-gateway login openai

# Release build — start daemon (default ports/paths)
cld-gateway serve

# Dev build — login with custom paths
GATEWAY_HOME=~/.gateway-dev cld-gateway-dev login openai

# Dev build — start daemon (different port and paths)
CLD_GATEWAY_LISTEN_ADDR=127.0.0.1:8081 \
GATEWAY_HOME=~/.gateway-dev \
cld-gateway-dev serve
```

---

## Supported endpoints

| Method | Path | Description |
|---|---|---|
| `GET` | `/health` | Health check |
| `GET` | `/auth/status` | Auth status |
| `POST` | `/auth/refresh` | Force auth token refresh |
| `GET` | `/v1/models` | List models (Anthropic-compatible) |
| `POST` | `/v1/messages` | Create message (Anthropic-compatible, streaming supported) |

---

## Authentication

### Supported methods

The `cld-gateway login openai` command supports two authentication methods:

1. **ChatGPT OAuth (recommended)**
   - Opens your browser to authenticate with OpenAI
   - Provides an access token, refresh token, and account ID
   - Required for `/v1/messages` endpoint access

2. **OpenAI API Key**
   - Paste your OpenAI API key directly in the TUI prompt
   - Enables `/v1/models` endpoint access
   - Note: `/v1/messages` still requires ChatGPT OAuth regardless of which method you choose

### Shared auth store

Both `cld-gateway login` and `cld-gateway serve` use the same auth file:

```
~/.gateway/auth.json
```

- **`cld-gateway login openai`** writes credentials to this file
- **`cld-gateway serve`** reads and uses credentials from this file
- Token refresh happens automatically in the daemon; no manual refresh needed

### Configuration and overrides

Override the auth file location using environment variables:

- `GATEWAY_AUTH_JSON_PATH`: Full path to auth.json
- `GATEWAY_HOME`: Directory containing auth.json (default: `~/.gateway`)

Example:

```sh
export GATEWAY_HOME=~/.gateway-custom
cld-gateway login openai      # Writes to ~/.gateway-custom/auth.json
cld-gateway serve             # Reads from ~/.gateway-custom/auth.json
```

### OAuth callback port

During ChatGPT OAuth, the gateway opens a local HTTP server to receive the callback. The port is selected as follows:

1. **Preferred:** `CLD_GATEWAY_AUTH_PORT` (default `1455`)
2. **Fallback:** If binding fails, automatically tries port `1457`

If you know port 1455 is occupied, set `CLD_GATEWAY_AUTH_PORT` to an available port before login:

```sh
CLD_GATEWAY_AUTH_PORT=2000 cld-gateway login openai
```

---

## Logs and debugging

Exchange logs are written to:

```
~/.gateway/logs/http-exchange.jsonl
```

Every proxied request receives an `x-proxy-request-id` response header that you can use to correlate request and response entries in the log.

To follow the log in real time:

```sh
tail -f ~/.gateway/logs/http-exchange.jsonl
```

If the log shows a `backend_error` or transport failure, the issue is upstream of the gateway (connectivity or auth). Start there before looking at request translation.

---

## Unsupported features

Some Anthropic API fields are accepted and parsed by the gateway but intentionally not forwarded to the upstream backend. See [UNSUPPORTED.md](UNSUPPORTED.md) for the full list and rationale.

Current no-ops include `top_k` and `stop_sequences`.

---

## Building from source

Release binary:

```sh
cargo build --release -p gatewayd --bin cld-gateway
```

Run locally (with checks):

```sh
make check && cargo run -p gatewayd --bin cld-gateway
```
