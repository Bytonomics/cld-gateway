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

## Running the gateway

```sh
cld-gateway
```

On first run, `cld-gateway` will prompt for an authentication method (ChatGPT OAuth or API key) and store credentials at `~/.gateway/auth.json` for reuse on subsequent runs.

See [Auth](#auth) for details on authentication methods and how to configure them.

### Environment variables

| Variable | Default | Description |
|---|---|---|
| `CLD_GATEWAY_LISTEN_ADDR` | `127.0.0.1:8080` | Listen address and port |
| `GATEWAY_HOME` | `~/.gateway` | Override all default `~/.gateway` paths at once |
| `GATEWAY_AUTH_JSON_PATH` | `~/.gateway/auth.json` | Auth credentials file path |
| `CLD_GATEWAY_AUTH_PORT` | `1455` | OAuth callback port (see below) |
| `CLD_GATEWAY_LOG_PATH` | `~/.gateway/logs/http-exchange.jsonl` | Exchange log file path |
| `CLD_GATEWAY_STATE_DB_PATH` | `~/.gateway/state/tool_calls.sqlite` | Tool-call state DB path |
| `GATEWAY_CONFIG_PATH` | `~/.gateway/config.json` | Gateway config file path |

### OAuth Callback Port Selection

During the ChatGPT login flow, `cld-gateway` opens a local HTTP server to receive the OAuth callback. Port selection follows this order:

1. **Preferred port:** The value of `CLD_GATEWAY_AUTH_PORT` (default `1455`)
2. **Fallback port:** If binding the preferred port fails, the gateway automatically falls back to port `1457`

Login succeeds as long as the gateway can bind one of those ports and the resulting localhost callback URL is reachable in the browser. If you know port 1455 is occupied (for example by another local service or another gateway instance), set `CLD_GATEWAY_AUTH_PORT` to an available port before starting the login flow.

---

## Side-by-side dev and release

You can run a dev build and a release build simultaneously by pointing each at different ports and data directories:

```sh
# Release build (default ports/paths)
cld-gateway

# Dev build (different port and paths)
CLD_GATEWAY_LISTEN_ADDR=127.0.0.1:8081 \
GATEWAY_HOME=~/.gateway-dev \
cld-gateway-dev
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

## Auth

`cld-gateway` supports two authentication methods:

### ChatGPT OAuth (Recommended)

On first run, `cld-gateway` interactively prompts for an authentication method. Selecting ChatGPT OAuth opens a browser window to authenticate with OpenAI. After completing login, credentials are stored at `~/.gateway/auth.json` for reuse on subsequent runs.

This method is required for `/v1/messages` endpoint access. It provides an access token, refresh token, and account ID.

**Auth flow:**
1. On startup, `cld-gateway` checks for existing valid credentials at `~/.gateway/auth.json`
2. If credentials exist and are valid, the gateway validates them with the upstream backend (this may refresh expired tokens)
3. If credentials are missing, expired, or invalid, an interactive login prompt appears
4. On successful login, credentials are persisted and the gateway starts listening

**Token refresh:** On subsequent runs, stored credentials are loaded automatically. If a request to the upstream backend returns a 401, the gateway refreshes the token automatically and retries the request — no manual intervention needed.

### OpenAI API Key (Alternative)

Alternatively, set `GATEWAY_FORCED_LOGIN_METHOD=api` to provide an OpenAI API key:

```sh
GATEWAY_FORCED_LOGIN_METHOD=api cld-gateway
```

When prompted, paste your OpenAI API key. This method enables `/v1/models` endpoint access (requires a valid OpenAI API key). However, the `/v1/messages` endpoint still requires ChatGPT OAuth regardless of which authentication method is configured.

When ChatGPT OAuth credentials are missing or invalid, the installed daemon currently requires an interactive login on startup. If you are running `cld-gateway` behind Claude Code, make sure the login flow has been completed successfully in the same environment so the daemon and subsequent requests can reuse the same `~/.gateway/auth.json`.

### Configuration

To override the auth file location, set:
- `GATEWAY_AUTH_JSON_PATH`: Full path to auth.json
- `GATEWAY_HOME`: Directory containing auth.json (default: `~/.gateway`)

To force a specific login method on every startup:
- `GATEWAY_FORCED_LOGIN_METHOD=chatgpt`: Always use ChatGPT OAuth
- `GATEWAY_FORCED_LOGIN_METHOD=api`: Always prompt for API key

The OAuth callback port can be customized via `CLD_GATEWAY_AUTH_PORT` (see [OAuth Callback Port Selection](#oauth-callback-port-selection) above).

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
