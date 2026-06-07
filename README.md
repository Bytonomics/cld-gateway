# cld-gateway

An Anthropic-compatible HTTP proxy that routes requests through the ChatGPT/Codex backend.

---

## Installation

### Homebrew tap

```sh
brew tap bytonomics/tap
brew install cld-gateway
```

Homebrew installs:
- the `cld-gateway` daemon binary
- wrapper commands `cldg` and `clddg`
- runtime config at `~/.gateway/config.yaml`
- Claude settings at `~/.claude_codex/settings.json`
- symlinks from `~/.claude_codex` to existing shared Claude Code entries under `~/.claude`

Before using the Homebrew wrappers, ensure the `claude` executable is already available on your `PATH` because `cldg` and `clddg` shell out to `claude`.

> **Note:** Homebrew availability depends on the separate `bytonomics/homebrew-tap` repo being updated for the release you want to install. Stable releases are intended to flow there automatically from the gateway release workflow.

### Shell installer

```sh
curl -fsSL https://github.com/Bytonomics/cld-gateway/releases/latest/download/install.sh | sh
```

Or with version pinning:

```sh
curl -fsSL https://github.com/Bytonomics/cld-gateway/releases/latest/download/install.sh | sh -s -- --release 0.1.1
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
cld-gateway login openai
```

### 2. Start the daemon

```sh
cld-gateway serve
```

The daemon listens on the address configured in `~/.gateway/config.yaml` (or `GATEWAY_CONFIG_PATH`/`GATEWAY_HOME`), defaulting to `127.0.0.1:8080` when no listen address is configured, and automatically handles token refresh.

If you see an auth error, run `cld-gateway login` again.

---

## Commands

| Command | Description |
|---|---|
| `cld-gateway` or `cld-gateway serve` | Start the daemon |
| `cld-gateway login` | Interactive login menu |
| `cld-gateway login openai` | ChatGPT browser login |
| `cld-gateway login gemini` | Gemini login (not yet implemented) |

---

## Future / Not Implemented

### Gemini login

`cld-gateway login gemini` is recognized but not yet implemented. Currently only ChatGPT OAuth is supported.

### Windows support

The gateway currently runs on Linux and macOS. Windows support is planned for a future release.

---

## Environment variables

| Variable | Default | Description |
|---|---|---|
| `GATEWAY_HOME` | `~/.gateway` | Override all default `~/.gateway` paths at once |
| `GATEWAY_AUTH_JSON_PATH` | `~/.gateway/auth.json` | Auth credentials file path |
| `CLD_GATEWAY_AUTH_PORT` | `1455` | OAuth callback port (see Authentication section) |
| `CLD_GATEWAY_LOG_PATH` | `~/.gateway/logs/http-exchange.jsonl` | Exchange log file path |
| `CLD_GATEWAY_STATE_DB_PATH` | `~/.gateway/state/tool_calls.sqlite` | Tool-call state DB path |
| `GATEWAY_CONFIG_PATH` | `~/.gateway/config.yaml` | Gateway config file path (including `network.listen_addr`) |

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

# Dev build — start daemon (different config path and data directory)
cat > ~/.gateway-dev/config.yaml <<'EOF'
network:
  listen_addr: 127.0.0.1:8081
EOF
GATEWAY_CONFIG_PATH=~/.gateway-dev/config.yaml \
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
