# cld-gateway

An Anthropic-compatible HTTP proxy that routes requests through the ChatGPT/Codex backend.

---

## Installation

### Homebrew tap

```sh
brew tap bytonomics/tap
brew install cld-gateway
```

> **Note:** Homebrew availability depends on the separate `bytonomics/homebrew-tap` repo being updated for the release you want to install. Stable releases are intended to flow there automatically from the gateway release workflow.

### Shell installer

```sh
curl -fsSL https://github.com/Bytonomics/cld-gateway/releases/latest/download/install.sh | sh
```

Or with version pinning:

```sh
curl -fsSL https://github.com/Bytonomics/cld-gateway/releases/latest/download/install.sh | sh -s -- --release 0.1.0
```

### Direct download

Download pre-built binaries from the [GitHub Releases page](https://github.com/Bytonomics/cld-gateway/releases).

Verify checksums using `cld-gateway-package_SHA256SUMS`, which is published alongside every release.

---

## Quick start

### 1. Log in (one-time setup)

```sh
cld-gateway login
```

This displays an interactive menu to choose your login method (ChatGPT, API Key, or Gemini).
Select ChatGPT to authenticate via browser. Credentials are saved to `~/.gateway/auth.json`.

For ChatGPT login directly without the menu:

```sh
cld-gateway login claude
```

### 2. Start the daemon

```sh
cld-gateway serve
```

The daemon runs on `127.0.0.1:8080` and automatically handles token refresh.

If you see an auth error, run `cld-gateway login` again.

---

## Commands

| Command | Description |
|---|---|
| `cld-gateway` or `cld-gateway serve` | Start the daemon |
| `cld-gateway login` | Interactive login menu |
| `cld-gateway login claude` | ChatGPT/Claude browser login |
| `cld-gateway login gemini` | Gemini login (not yet implemented) |

---

## Future / Not Implemented

### Gemini login

`cld-gateway login gemini` is recognized but not yet implemented. Currently only ChatGPT/Claude OAuth is supported.

### Windows support

The gateway currently runs on Linux and macOS. Windows support is planned for a future release.

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
cld-gateway login claude

# Release build — start daemon (default ports/paths)
cld-gateway serve

# Dev build — login with custom paths
GATEWAY_HOME=~/.gateway-dev cld-gateway-dev login claude

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

Credentials are saved to `~/.gateway/auth.json`. To use a custom location, set `GATEWAY_HOME` or `GATEWAY_AUTH_JSON_PATH` before running login or serve.

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
