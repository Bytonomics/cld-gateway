# cld-gateway

An Anthropic-compatible HTTP proxy that routes requests through the ChatGPT/Codex backend.

---

## Requirements

- `claude` must already be installed and available on your `PATH`

## Installation

### Homebrew tap

```sh
brew tap bytonomics/tap
brew install cld-gateway
```

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

## What installation sets up

A Homebrew install sets up:

- the `cld-gateway` daemon binary
- runtime config at `~/.gateway/config.yml`
- Claude settings at `~/.claude_codex/settings.json`
- wrapper commands `cldg` and `clddg`
- symlinks from `~/.claude_codex` to existing shared Claude Code entries under `~/.claude`

The `cldg` and `clddg` wrappers shell out to `claude`, so the `claude` executable must already be available on your
`PATH` before you use those wrappers.

---

## Homebrew service setup

```sh
brew services start cld-gateway
brew services stop cld-gateway
brew services restart cld-gateway
brew services list
```

The Homebrew service runs `cld-gateway serve` and uses `~/.gateway/config.yml` as its runtime config file.

---

## Quick start

### 1. Log in (one-time setup)

```sh
cld-gateway login
```

For explicit vendor selection:

```sh
cld-gateway login openai
cld-gateway login gemini
```

### 2. Start the daemon

```sh
cld-gateway serve
```

The daemon listens on the address configured in `~/.gateway/config.yml`. If no listen address is configured, it defaults
to `127.0.0.1:8080` and automatically handles token refresh.

If you see an auth error, run `cld-gateway login` again.

---

## Config file

The Homebrew-installed daemon uses this runtime config file:

```text
~/.gateway/config.yml
```

Minimal example:

```yaml
version: 1
providers:
  openai:
    default_model: gpt-5.4
    unsupported_models:
      - gpt-5.2
workflow:
  fast_mode: false
network:
  listen_addr: 127.0.0.1:6473
```

| Key                                   | Type         | Default          | What it does                                   |
|---------------------------------------|--------------|------------------|------------------------------------------------|
| `version`                             | integer      | `1`              | Config schema version                          |
| `providers.openai.default_model`      | string       | `gpt-5.4`        | Backend model used for compatibility overrides |
| `providers.openai.unsupported_models` | list[string] | `['gpt-5.2']`    | Models that are rewritten to `default_model`   |
| `workflow.fast_mode`                  | boolean      | `false`          | Sends `service_tier: priority` to the backend  |
| `network.listen_addr`                 | string       | `127.0.0.1:6473` | Socket address that the daemon binds to        |
| `network.allowed_hosts`               | list[string] | `[]`             | Reserved list of allowed host names            |

---

## Wrapper commands

- `cldg` runs `claude --settings ~/.claude_codex/settings.json`
- `clddg` runs `cldg --dangerously-skip-permissions`

Both wrappers require `claude` to already be installed and available on your `PATH`.

---

## Supported endpoints

| Method | Path            | Description                                                |
|--------|-----------------|------------------------------------------------------------|
| `GET`  | `/health`       | Health check                                               |
| `GET`  | `/auth/status`  | Auth status                                                |
| `POST` | `/auth/refresh` | Force auth token refresh                                   |
| `GET`  | `/v1/models`    | List models (Anthropic-compatible)                         |
| `POST` | `/v1/messages`  | Create message (Anthropic-compatible, streaming supported) |

---

## Troubleshooting / logs

Credentials are saved to `~/.gateway/auth.json`.

Exchange logs are written to:

```text
~/.gateway/logs/http-exchange.jsonl
```

To follow the log in real time:

```sh
tail -f ~/.gateway/logs/http-exchange.jsonl
```

The default auth callback port is `1455`. If that port is unavailable, the gateway falls back to `1457`. You can
override the preferred callback port with `CLD_GATEWAY_AUTH_PORT`.

---

## Custom gateway behavior (it's not just API translation proxy, but there are many custom changes to make the gateway work with claude code and open AI seamlessly)

| Serial no | 10-word title                                                                      | What changed                                                                                                                                                                                  | Why it exists                                                                                                             |
|-----------|------------------------------------------------------------------------------------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|---------------------------------------------------------------------------------------------------------------------------|
| 1         | Anthropic system prompts and histories are normalized into backend instructions    | Anthropic `system[]` blocks and message history are collapsed and reshaped into the backend request format with `instructions` and `input` items.                                             | The backend expects a different conversational structure than Anthropic’s messages API.                                   |
| 2         | Tool calls are translated into typed backend operations and results                | Anthropic `tool_use` and `tool_result` blocks are converted into typed backend call and result items such as `function_call`, `custom_tool_call`, `tool_search_call`, and `local_shell_call`. | The gateway must preserve tool semantics across incompatible tool protocols.                                              |
| 3         | Hosted web search is remapped into backend search tooling semantics                | Anthropic hosted web search tools are translated into backend `web_search` tools with filters, required tool choice, and extra include fields.                                                | The backend and Anthropic expose search differently, so the gateway must adapt both schema and execution semantics.       |
| 4         | Streaming backend events become Anthropic-style SSE blocks with preserved state    | Backend streaming events are turned into Anthropic SSE events like `message_start`, `content_block_start`, `content_block_delta`, `message_delta`, and `message_stop`.                        | Claude-facing clients expect Anthropic SSE semantics, not raw backend event families.                                     |
| 5         | Gateway edits context history before forwarding requests to backend safely         | The gateway can prune or rewrite thinking and tool history using configured context-management policies and hard limits before forwarding requests.                                           | This provides controllable context trimming and compatibility behavior beyond simple pass-through.                        |
| 6         | Unsupported models are rewritten to configured defaults for compatibility handling | Requested models in the configured unsupported list are transparently remapped to a configured default backend model.                                                                         | This keeps clients working even when the backend cannot serve particular requested models.                                |
| 7         | Fast mode injects priority service tier into backend request payloads              | `workflow.fast_mode` causes the gateway to inject backend `service_tier: "priority"` into outbound requests.                                                                                  | This adds a gateway-owned runtime policy knob rather than requiring clients to know backend-specific service tier fields. |
| 8         | Auth refresh retries and logout logic protect long-running sessions reliably       | Backend `401` responses trigger one refresh-and-retry attempt, and permanent refresh failure can revoke and clear auth state.                                                                 | This keeps long-running gateway sessions healthier and makes auth recovery automatic.                                     |

---

## Future work

### Windows support

The gateway currently runs on Linux and macOS. Windows support is planned for a future release.

### Gemini support

`cld-gateway login gemini` is recognized but not yet implemented. Currently only the ChatGPT/OpenAI login flow is
supported.

---
