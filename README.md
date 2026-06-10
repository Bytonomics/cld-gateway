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
curl -fsSL https://github.com/Bytonomics/cld-gateway/releases/latest/download/install.sh | sh -s -- --release 0.1.3
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
  claude_code:
    slash_commands:
      enabled: true
      mode: promote_latest
network:
  listen_addr: 127.0.0.1:6473
  allowed_hosts: [ ]
```

The values below are loaded from YAML into typed Rust config structs unless marked as an environment variable. Omitted
YAML fields use code defaults. The packaged Homebrew config can still choose different values, such as
`network.listen_addr: 127.0.0.1:6473`.

### Root config

| No | Config    | Value / example | Behavior                                               | How to choose                                                 |
|----|-----------|-----------------|--------------------------------------------------------|---------------------------------------------------------------|
| 1  | `version` | Purpose         | Config schema version.                                 | Keep it in the file so future migrations can be explicit.     |
|    |           | `1`             | Current supported schema.                              | Use this for all current configs.                             |
|    |           | `2`, `3`, ...   | Reserved; future versions may change schema semantics. | Do not set manually until the gateway documents that version. |

### `providers.openai`

| No | Config                                | Value / example             | Behavior                                                                                      | How to choose                                                                                    |
|----|---------------------------------------|-----------------------------|-----------------------------------------------------------------------------------------------|--------------------------------------------------------------------------------------------------|
| 1  | `providers.openai.default_model`      | Purpose                     | Backend model used when a requested model is listed in `providers.openai.unsupported_models`. | This is the compatibility fallback, not a general model alias system.                            |
|    |                                       | `gpt-5.4`                   | Uses the standard backend model as the fallback.                                              | Use when quality/default behavior matters more than cost.                                        |
|    |                                       | `gpt-5.4-mini`              | Uses a cheaper/smaller backend model as the fallback.                                         | Use only if unsupported aliases should intentionally fall back to a lower-cost model.            |
| 2  | `providers.openai.unsupported_models` | Purpose                     | Requested model names in this list are rewritten to `providers.openai.default_model`.         | Add only model IDs that the backend rejects or that you intentionally want centrally redirected. |
|    |                                       | `[]`                        | No compatibility rewrites.                                                                    | Use only when every Claude Code model name you send is accepted by the backend.                  |
|    |                                       | `[gpt-5.2]`                 | Requests for `gpt-5.2` are sent as `default_model` instead.                                   | This is the code default.                                                                        |
|    |                                       | `[gpt-5.2, some-old-alias]` | Multiple requested names are rewritten to `default_model`.                                    | Use during migrations from old client-side model names.                                          |

### `workflow`

| No | Config               | Value / example | Behavior                                                             | How to choose                                                         |
|----|----------------------|-----------------|----------------------------------------------------------------------|-----------------------------------------------------------------------|
| 1  | `workflow.fast_mode` | Purpose         | Controls whether Gateway asks the backend for priority service tier. | This is Gateway’s equivalent of a faster, higher-usage mode.          |
|    |                      | `true`          | Adds `service_tier: "priority"` to backend requests.                 | Pick when latency matters more than usage/cost.                       |
|    |                      | `false`         | Sends no service-tier override.                                      | Pick for normal usage and predictable cost; this is the code default. |

### `workflow.context_management`

| No | Config                                                               | Value / example                                                                                                | Behavior                                                                                        | How to choose                                                                        |
|----|----------------------------------------------------------------------|----------------------------------------------------------------------------------------------------------------|-------------------------------------------------------------------------------------------------|--------------------------------------------------------------------------------------|
| 1  | `workflow.context_management.enabled`                                | Purpose                                                                                                        | Master switch for Gateway-side context editing before Anthropic-to-OpenAI translation.          | Keep enabled for long Claude Code sessions unless debugging raw history.             |
|    |                                                                      | `true`                                                                                                         | Gateway resolves an effective edit list, applies supported edits, then applies hard limits.     | Default; use for normal operation.                                                   |
|    |                                                                      | `false`                                                                                                        | Gateway does no context pruning at all.                                                         | Use only when you need the full raw Claude Code request history forwarded unchanged. |
| 2  | `workflow.context_management.mode`                                   | Purpose                                                                                                        | Chooses whether Claude Code request edits or Gateway config edits control pruning.              | This mode only matters when `workflow.context_management.enabled: true`.             |
|    |                                                                      | `follow_request`                                                                                               | If Claude Code sends a non-empty `context_management.edits` list, use it; otherwise use `default_edits`. | Default; best match for Anthropic-compatible behavior with Gateway fallback policy.  |
|    |                                                                      | `override_request`                                                                                             | Ignore Claude Code request edits and use `override_edits` if present, otherwise an empty edit list. | Use when operators need a hard Gateway-owned pruning policy.                         |
|    |                                                                      | any other value                                                                                                | Config parse fails because the enum value is unsupported.                                       | Do not use values other than `follow_request` or `override_request`.                 |
| 3  | `workflow.context_management.default_edits`                          | Purpose                                                                                                        | Context edit list decoded as Anthropic-style edit objects and used only when `mode: follow_request` and Claude Code sends no non-empty request edit list. | Use for a Gateway fallback policy without overriding Claude Code.                    |
|    |                                                                      | `[]`                                                                                                           | No default pruning edits.                                                                       | Default; use when Claude Code normally provides its own policy.                      |
|    |                                                                      | `[{type: clear_tool_uses_20250919, trigger: {type: tool_uses, value: 20}, keep: {type: tool_uses, value: 5}}]` | After more than 20 tool interactions, keep only the 5 newest eligible tool interactions.        | Good fallback for sessions that generate lots of tool output.                        |
| 4  | `workflow.context_management.override_edits`                         | Purpose                                                                                                        | Optional replacement edit list decoded as Anthropic-style edit objects and used only when `mode: override_request`. | Use when Gateway policy must override Claude Code’s requested policy.                |
|    |                                                                      | `null` or omitted                                                                                              | No override edits; only `hard_limits` can prune when `mode: override_request`.                  | Default; do not use `override_request` with this unless hard limits are enough.      |
|    |                                                                      | `[{type: clear_thinking_20251015, keep: {type: thinking_turns, value: 1}}]`                                    | Always keeps only the newest assistant thinking turn.                                           | Use to cap stale thinking even if Claude Code asks for different pruning.            |
| 5  | `workflow.context_management.*_edits[].type`                         | Purpose                                                                                                        | Selects the edit implementation. Matching is prefix-based; date suffixes are tolerated.         | Configure inside `default_edits` or `override_edits`.                                |
|    |                                                                      | `clear_tool_uses_20250919` or any value starting `clear_tool_uses`                                             | Clears older tool results and optionally tool inputs.                                           | Use when terminal/search/tool outputs are bloating context.                          |
|    |                                                                      | `clear_thinking_20251015` or any value starting `clear_thinking`                                               | Clears older assistant `thinking` / `redacted_thinking` blocks.                                 | Use when stale thinking is making current instructions less dominant.                |
|    |                                                                      | invalid object or unknown prefix                                                                               | Invalid config edit objects and unsupported edit types are ignored and reported in context-management metadata. | Avoid; they will not prune anything.                                                  |
| 6  | `workflow.context_management.*_edits[].trigger.type`                 | Purpose                                                                                                        | Optional activation condition for `clear_tool_uses*` edits. Thinking edits do not use triggers. | Omit the trigger to always activate a `clear_tool_uses*` edit.                       |
|    |                                                                      | `tool_uses` with `value: 20`                                                                                   | Activates only when the request has more than 20 collected tool-use interactions.               | Use for tool-heavy agent sessions.                                                   |
|    |                                                                      | `input_tokens` with `value: 50000`                                                                             | Activates only when Gateway’s rough token estimate for the message list exceeds 50k.            | Use when pruning should be based on approximate prompt size, not raw tool count.     |
|    |                                                                      | unknown value                                                                                                  | Trigger is not active, so the edit does not run.                                                | Avoid unsupported trigger types.                                                     |
| 7  | `workflow.context_management.*_edits[].keep`                         | Purpose                                                                                                        | Controls how much recent history survives an edit.                                              | Tune this based on how much recent context the model still needs.                    |
|    |                                                                      | `{type: tool_uses, value: 5}`                                                                                  | For `clear_tool_uses*`, keeps the 5 newest eligible tool interactions.                          | Aggressive but useful for long tool-heavy sessions.                                  |
|    |                                                                      | `{type: thinking_turns, value: 1}`                                                                             | For `clear_thinking*`, keeps only the newest thinking turn.                                     | Good default when stale reasoning is hurting instruction following.                  |
|    |                                                                      | omitted for `clear_tool_uses*` / `clear_thinking*`                                                            | Keeps 3 newest tool interactions for `clear_tool_uses*`; keeps 1 newest thinking turn for `clear_thinking*`. | Use when the code defaults are acceptable.                                           |
|    |                                                                      | `"all"` for `clear_thinking*`                                                                                  | Disables that thinking edit because there is nothing to clear.                                  | Use only if you need to preserve all thinking blocks while keeping the edit shape.   |
| 8  | `workflow.context_management.*_edits[].clear_at_least`               | Purpose                                                                                                        | Optional minimum estimated token savings gate for `clear_tool_uses*` edits.                    | Use to avoid pruning when the savings would be too small to matter.                  |
|    |                                                                      | `{type: input_tokens, value: 2000}`                                                                            | Runs only if the edit can clear roughly 2k estimated tokens.                                    | Good conservative setting.                                                           |
|    |                                                                      | `{type: input_tokens, value: 10000}`                                                                           | Runs only for large context savings.                                                            | Use when you want to preserve history unless the request is very bloated.            |
|    |                                                                      | unknown type                                                                                                   | The minimum-savings gate is not applied, so the edit can still run if other conditions pass.    | Use `input_tokens`; other values are not useful today.                               |
| 9  | `workflow.context_management.*_edits[].exclude_tools`                | Purpose                                                                                                        | Exact tool names excluded from `clear_tool_uses*` clearing. Matching uses the tool-use block’s `name`. | Use for tools whose output must remain visible for correctness.                      |
|    |                                                                      | `[]`                                                                                                           | No named tools are excluded; all collected interactions are eligible.                           | Default; simplest policy.                                                            |
|    |                                                                      | `[Read, WebSearch]`                                                                                            | Keeps interactions from named tools out of the clearable set.                                   | Use if those tool outputs are important context anchors.                             |
| 10 | `workflow.context_management.*_edits[].clear_tool_inputs`            | Purpose                                                                                                        | Controls whether cleared tool calls also lose their input arguments.                            | Tool results are cleared either way; this only affects tool-use input payloads.      |
|    |                                                                      | `true`                                                                                                         | Clears both older tool results and older tool input arguments.                                  | Use for maximum context reduction.                                                   |
|    |                                                                      | `false`                                                                                                        | Clears older tool results but keeps tool input arguments.                                       | Default; use when knowing what was called matters.                                   |
| 11 | `workflow.context_management.hard_limits.max_tool_result_chars`      | Purpose                                                                                                        | After normal edits and other hard limits, clears any remaining individual tool result whose text/content exceeds this character limit. | Hard limit runs after normal context edits.                                          |
|    |                                                                      | omitted                                                                                                        | No per-tool-result character cap.                                                               | Default.                                                                             |
|    |                                                                      | `20000`                                                                                                        | Clears single tool results larger than about 20k chars.                                         | Moderate cap for large terminal/search outputs.                                      |
|    |                                                                      | `100000`                                                                                                       | Clears only extremely large single tool results.                                                | Permissive cap when preserving output is usually more important.                     |
| 12 | `workflow.context_management.hard_limits.max_tool_uses_to_keep`      | Purpose                                                                                                        | After normal edits, keeps only the newest N tool-use interactions and clears older tool results; tool input arguments are not cleared by this hard limit. | Use as an operator safety rail.                                                      |
|    |                                                                      | omitted                                                                                                        | No hard cap on tool interactions.                                                               | Default.                                                                             |
|    |                                                                      | `10`                                                                                                           | Keeps only the 10 newest tool interactions.                                                     | Aggressive cap for very long sessions.                                               |
|    |                                                                      | `30`                                                                                                           | Keeps the 30 newest tool interactions.                                                          | Balanced cap for preserving recent work.                                             |
| 13 | `workflow.context_management.hard_limits.max_thinking_turns_to_keep` | Purpose                                                                                                        | After normal edits, keeps only the newest N assistant messages containing `thinking` or `redacted_thinking` blocks. | Use when old thinking keeps steering new tasks.                                      |
|    |                                                                      | omitted                                                                                                        | No Gateway hard cap on thinking turns.                                                          | Default.                                                                             |
|    |                                                                      | `1`                                                                                                            | Keeps only the latest thinking turn.                                                            | Aggressive setting for strict latest-instruction adherence.                          |
|    |                                                                      | `3`                                                                                                            | Keeps the 3 latest thinking turns.                                                              | Balanced setting for recent reasoning context.                                       |

### `workflow.claude_code`

| No | Config                                        | Value / example  | Behavior                                                                                                                                                                                                                                                                                                  | How to choose                                                                                                                                 |
|----|-----------------------------------------------|------------------|-----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------|-----------------------------------------------------------------------------------------------------------------------------------------------|
| 1  | `workflow.claude_code.slash_commands.enabled` | Purpose          | Controls whether Gateway parses Claude Code’s expanded command envelope as structured instructions/input. Claude Code sends custom slash commands and skill invocations in the same over-the-wire format: `<command-message>`, `<command-name>`, and optional `<command-args>`.                         | Keep enabled so `/commit`, `/review_agent`, `/make-tasks-for-plan`, and skill-backed command envelopes follow their expanded instructions.    |
|    |                                               | `true`           | Enables one command-envelope translation pipeline. If the promoted command body starts with `Base directory for this skill:`, Gateway rewrites that line to append `analyze the files in this directory before proceeding` before promoting the same body.                                                | Default; normal gateway behavior.                                                                                                             |
|    |                                               | `false`          | Disables command-envelope translation and forwards command tags as ordinary text.                                                                                                                                                                                                                         | Use only for debugging raw Claude Code requests; OpenAI may focus on the command name and ignore the expanded body.                           |
| 2  | `workflow.claude_code.slash_commands.mode`    | Purpose          | Selects which command envelope is treated as active and how it is translated.                                                                                                                                                                                                                             | This only matters when `slash_commands.enabled: true`.                                                                                        |
|    |                                               | `promote_latest` | Only supported value. If the latest user message contains `<command-name>` / `<command-args>`, Gateway moves the expanded command body into backend `instructions`, sends only command args as user input, strips command tags from user input, and compacts older command envelopes as historical.        | Use this for Claude Code command envelopes; it mirrors Codex-style slash dispatch where instructions and user args are separated.             |
|    |                                               | any other value  | Config parse fails because the enum value is unsupported.                                                                                                                                                                                                                                                 | Do not set another mode until the gateway implements and documents it.                                                                        |

### `network`

| No | Config                  | Value / example                        | Behavior                                                                                  | How to choose                                                                                              |
|----|-------------------------|----------------------------------------|-------------------------------------------------------------------------------------------|------------------------------------------------------------------------------------------------------------|
| 1  | `network.listen_addr`   | Purpose                                | Socket address that `cld-gateway serve` binds to.                                         | Controls where Claude Code should point its Anthropic-compatible base URL.                                 |
|    |                         | `127.0.0.1:8080`                       | Code default when no config file supplies a value.                                        | Good for local-only development.                                                                           |
|    |                         | `127.0.0.1:6473`                       | Packaged Homebrew config value.                                                           | Good for avoiding common app ports while staying local-only.                                               |
|    |                         | `0.0.0.0:6473`                         | Binds on all interfaces.                                                                  | Use only when intentionally exposing the gateway on a LAN/container network with external access controls. |
| 2  | `network.allowed_hosts` | Purpose                                | Additional outbound hosts allowed by Gateway’s network policy.                            | Anthropic/Claude hosts are still blocked even if listed here.                                              |
|    |                         | `[]`                                   | No extra outbound hosts. Built-in OpenAI/ChatGPT auth hosts and localhost remain allowed. | Default; safest setting.                                                                                   |
|    |                         | `[example.com]`                        | Allows one extra non-Anthropic host.                                                      | Use for a single future integration endpoint.                                                              |
|    |                         | `[api.github.com, uploads.github.com]` | Allows multiple extra non-Anthropic hosts.                                                | Use when a future Gateway integration needs multiple known hosts.                                          |

### Environment variables

| No | Config                                 | Value / example                     | Behavior                                                                                                                         | How to choose                                                                                       |
|----|----------------------------------------|-------------------------------------|----------------------------------------------------------------------------------------------------------------------------------|-----------------------------------------------------------------------------------------------------|
| 1  | `GATEWAY_CONFIG_PATH`                  | Purpose                             | Exact YAML config file path.                                                                                                     | Highest-priority config path override.                                                              |
|    |                                        | unset                               | Gateway falls back to `GATEWAY_HOME/config-dev.yml`, then `~/.gateway/config-dev.yml`.                                           | Normal for local development.                                                                       |
|    |                                        | `/Users/me/.gateway/config.yml`     | Loads that exact file.                                                                                                           | Homebrew services use this to point at the installed runtime config.                                |
| 2  | `GATEWAY_HOME`                         | Purpose                             | Gateway home directory when more specific path vars are unset.                                                                   | Centralizes config, auth, logs, keyring identity, and state paths.                                  |
|    |                                        | unset                               | Uses `~/.gateway` for most runtime files.                                                                                        | Normal single-user setup.                                                                           |
|    |                                        | `/tmp/gateway-dev`                  | Uses `/tmp/gateway-dev/config-dev.yml`, `/tmp/gateway-dev/auth.json`, logs, and state paths unless more specific vars override them. | Useful for isolated dev/test runs.                                                                  |
| 3  | `GATEWAY_AUTH_JSON_PATH`               | Purpose                             | Exact auth JSON path.                                                                                                            | Overrides `GATEWAY_HOME/auth.json` and `~/.gateway/auth.json`.                                      |
|    |                                        | unset                               | Uses `GATEWAY_HOME/auth.json` or `~/.gateway/auth.json`.                                                                         | Normal setup after `cld-gateway login`.                                                             |
|    |                                        | `/tmp/gateway-auth.json`            | Reads/writes auth at that exact file.                                                                                            | Useful for isolated test credentials.                                                               |
| 4  | `OPENAI_API_KEY`                       | Purpose                             | API key used by `/v1/models` before falling back to an API key stored in Gateway auth JSON.                                      | This does not replace the message credential path, which still reads Gateway auth state.            |
|    |                                        | unset                               | `/v1/models` tries `OPENAI_API_KEY` stored in `~/.gateway/auth.json` or the configured auth path.                                | Normal after API-key login/persistence.                                                             |
|    |                                        | `sk-...`                            | `/v1/models` uses this key directly for the upstream model-list request.                                                         | Use in CI or environments where file-based API-key auth is not available.                           |
| 5  | `GATEWAY_BACKEND_REQUEST_TIMEOUT_SECS` | Purpose                             | Total backend request timeout.                                                                                                   | Applies to backend OpenAI/Codex requests.                                                           |
|    |                                        | unset                               | No gateway-imposed total request timeout.                                                                                        | Default.                                                                                            |
|    |                                        | `0`                                 | Disables the timeout, same as unset.                                                                                             | Use if a service manager injects the var but you want no timeout.                                   |
|    |                                        | `60` or `600`                       | Times out backend requests after 60 seconds or 10 minutes.                                                                       | Use shorter values for interactive fail-fast behavior; longer values for long tool/search sessions. |
|    |                                        | non-number                          | Ignored with a warning.                                                                                                          | Avoid; it behaves like unset.                                                                       |
| 6  | `GATEWAY_ALLOWED_OUTBOUND_HOSTS`       | Purpose                             | Comma-separated outbound host allowlist entries appended before YAML `network.allowed_hosts` is applied.                         | Use for runtime overrides when editing YAML is inconvenient.                                        |
|    |                                        | unset                               | Use built-in allowed hosts plus `network.allowed_hosts`.                                                                         | Normal setting.                                                                                     |
|    |                                        | `api.github.com,uploads.github.com` | Allows those extra non-Anthropic hosts.                                                                                          | Useful for temporary integration testing.                                                           |
| 7  | `CLD_GATEWAY_LOG_PATH`                 | Purpose                             | Exact HTTP exchange log file path.                                                                                               | Overrides `GATEWAY_HOME/logs/http-exchange.jsonl` and `~/.gateway/logs/http-exchange.jsonl`.        |
|    |                                        | unset                               | Logs to `GATEWAY_HOME/logs/http-exchange.jsonl` or `~/.gateway/logs/http-exchange.jsonl`.                                        | Normal setup.                                                                                       |
|    |                                        | `/tmp/gateway-http.jsonl`           | Writes exchange logs to that file.                                                                                               | Useful for isolated debugging runs.                                                                 |
| 8  | `CLD_GATEWAY_STATE_DB_PATH`            | Purpose                             | Exact SQLite state DB path used for tool-call metadata.                                                                          | Overrides `GATEWAY_HOME/state/tool_calls.sqlite`.                                                   |
|    |                                        | unset                               | Uses `GATEWAY_HOME/state/tool_calls.sqlite` or `~/.gateway/state/tool_calls.sqlite`.                                             | Normal setup.                                                                                       |
|    |                                        | `/tmp/gateway-tool-calls.sqlite`    | Stores tool-call state in that SQLite file.                                                                                      | Useful for tests or isolated daemon instances.                                                      |
| 9  | `CLD_GATEWAY_AUTH_PORT`                | Purpose                             | Preferred local OAuth callback port during login.                                                                                | If unavailable, login falls back to port `1457`.                                                    |
|    |                                        | unset                               | Uses preferred port `1455`, then fallback `1457`.                                                                                | Normal setup.                                                                                       |
|    |                                        | `1456` or `18080`                   | Tries that port first, then falls back to `1457` if busy.                                                                        | Use when `1455` conflicts with another local service.                                               |

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
