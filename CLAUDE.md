# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Absolute rule: never depend on prompt text

Gateway code can NEVER depend on the text content of prompts (system or
user messages) — not even if that seems like the only way to support a
feature or fix a bug. It is not to be considered. Any decision that sniffs
prompt wording (e.g. `contains("some phrase")` over message text) is a bug.

Message SIZE or SHAPE heuristics (small/large message count, short/long
request, role-mix patterns used as a proxy for intent) are equally
forbidden — they are the same dependence wearing a different costume.

Decisions must use deterministic, structured signals only:
- message tags the client emits from code (`<command-message>`,
  `<command-name>`, `<command-args>`, `<local-command-stdout>`)
- client metadata fields (e.g. `gateway_conversation_inclusion`)
- explicit request fields (stream flag, output config, tool_choice)

Known violations live in `golang_port/docs/AI_SLOP.md`. Do not port them;
do not add new ones.

## Quick commands (Rust workspace)

This repo is a Rust workspace (`Cargo.toml` is a virtual workspace manifest) with a repo-level `Makefile`.

### Format / lint / tests

- Full local verification (fmt-check + clippy + tests):
  - `make check`

- Formatting only:
  - `make fmt-check`
  - `make fmt-fix`

- Clippy only:
  - `make clippy`

- All tests (workspace):
  - `make test`

- Run wiremock-gated integration tests (some tests early-return unless this is set):
  - `RUN_WIREMOCK=1 make verify-test`

### Run a single test

Use `cargo test` directly for narrower selection:

- Single crate tests:
  - `cargo test -p gateway-http-anthropic`

- Single test by name substring (example):
  - `cargo test -p gateway-http-anthropic streaming_bridge_matches_text_only_fixture`

(See `crates/gateway-http-anthropic/src/sse_bridge.rs` and `crates/gateway-http-anthropic/src/lib.rs` for existing test names.)

### Pre-commit hooks

Pre-commit is configured in `.pre-commit-config.yaml` and runs:

- basic hygiene: trailing whitespace, EOF fixer, yaml/toml checks
- `make check`

If a commit fails because a hook modified files, re-stage the hook-changed files and re-run `git commit`.

## README editing discipline

- Treat `README.md` and `homebrew-tap/README.md` as intentionally curated docs with a fixed detail level.
- Do not add new sections, new categories of information, or a deeper level of implementation detail unless the user explicitly asks for that expansion.
- When updating a README, preserve its existing information architecture and tone; make the smallest change that keeps it correct.
- Keep end-user README content focused on setup, usage, and user-visible behavior.
- Keep maintainer/release-engineering details out of README files unless the user explicitly asks for them there.
- If a README needs more detail than its current structure can support, stop and ask the user before expanding it.

## Codebase structure (big picture)

### Runtime topology

`gatewayd` is the runnable daemon that wires everything together:

- Binds an Axum HTTP server to `network.listen_addr` from gateway config, defaulting to `127.0.0.1:6483`.
- Performs a non-interactive OpenAI auth preflight in `serve` mode; auth failures abort startup.
- Runs interactive auth only through `cld-gateway login [openai|gemini]`.
- Wraps the router with observability middleware that logs exchanges.

Key entrypoint:
- `crates/gatewayd/src/main.rs`

### Crates and responsibilities

The workspace is organized as small crates with explicit “allowed/not allowed” boundaries documented at the top of each crate’s `lib.rs`.

- `crates/gateway-core`
  - Shared types/utilities (`RequestId`, `Secret<T>`, gateway runtime config).
  - Gateway-owned defaults, unsupported-model compatibility, and workflow toggles are in `gateway_core::config`.

- `crates/gateway-auth-codex`
  - Loads/refreshes Codex OAuth credentials from disk.
  - Exposes safe snapshots/status types (does not expose secrets directly).
  - Default auth path resolution (`~/.gateway/auth.json`, or overrides via env vars).
  - Key files:
    - `crates/gateway-auth-codex/src/lib.rs`
    - `crates/gateway-auth-codex/src/paths.rs`

- `crates/gateway-backend-codex`
  - HTTP client for the upstream ChatGPT/Codex backend endpoint:
    - `POST {base_url}/backend-api/codex/responses` (default `https://chatgpt.com`).
  - Uses HTTP SSE (`Accept: text/event-stream`) for full request transport.
  - Supports reusable Codex Responses WebSocket sessions for incremental/delta transport, including refresh+retry on 401.
  - Key file:
    - `crates/gateway-backend-codex/src/client.rs`

- `crates/gateway-http-anthropic`
  - The Anthropic-compatible HTTP surface area.
  - Routes:
    - `GET /health`
    - `GET /auth/status`
    - `POST /auth/refresh`
    - `GET /v1/models`
    - `POST /v1/messages`
  - Responsibilities:
    - Parse Anthropic-ish request types (`types.rs`).
    - Translate requests into Codex backend request shape (`translate.rs`).
    - Bridge backend SSE/WebSocket event streams into Anthropic streaming events (`sse_bridge.rs`).
    - Apply Claude Code context normalization, slash-command translation, and Gateway context-management edits.
    - Select conversation branches, reconcile checkpoints, choose full vs incremental transport, reuse per-branch WebSocket sessions, and commit provider checkpoints through `gateway-state`.
    - Persist tool-call IDs/correlation via `gateway-state`.
  - Key file:
    - `crates/gateway-http-anthropic/src/lib.rs`

- `crates/gateway-state`
  - Local persistence for gateway runtime state.
  - Stores tool-call metadata/correlation in SQLite (`ToolCallStore`) under `~/.gateway/state/tool_calls.sqlite` by default.
  - Stores conversation-state sessions, branches, sparse checkpoints, OpenAI provider checkpoints, fingerprints, reconciliation metadata, compaction/reset state, corruption policy, and retention cleanup under the configured conversation-state root.
  - Key files:
    - `crates/gateway-state/src/lib.rs`
    - `crates/gateway-state/src/conversation.rs`
    - `crates/gateway-state/src/tool_calls.rs`

- `crates/gateway-net`
  - Central outbound network policy and `reqwest` wrapper.
  - Allows default OpenAI/ChatGPT hosts, localhost, and configured additional hosts.
  - Blocks Anthropic/Claude hosts even if configured as allowed.
  - Key file:
    - `crates/gateway-net/src/lib.rs`

- `crates/gateway-observability`
  - Request/response capture middleware (no business logic).
  - Writes exchange logs to:
    - `~/.gateway/logs/http-exchange.jsonl`
  - Adds `x-proxy-request-id` header to responses for correlation.
  - Key files:
    - `crates/gateway-observability/src/middleware.rs`
    - `crates/gateway-observability/src/paths.rs`

### Auth + configuration locations

- Developer mode (`cldc` / `clddc`):
  - `cldc` is a shell alias for `claude --settings ~/.claude_codex/settings.json`.
  - `clddc` is a shell alias for `cldc --dangerously-skip-permissions`.
  - This path is for the cargo-run gateway on `http://127.0.0.1:6483`.
  - Claude model aliases and the `/model` menu for this mode come from `~/.claude_codex/settings.json`.
  - Runtime gateway config for cargo-run mode defaults to `~/.gateway/config-dev.yml` unless `GATEWAY_CONFIG_PATH` or `GATEWAY_HOME` is set.
  - Do not inspect `~/.gateway/config.yml` when debugging `cldc` / `clddc` model resolution unless the process environment explicitly sets `GATEWAY_CONFIG_PATH` to that file.
  - When changing model defaults or unsupported-model lists for development, update both `~/.claude_codex/settings.json` and `~/.gateway/config-dev.yml`.

- Homebrew/package mode (`cldg` / `clddg`):
  - `cldg` runs `claude --settings ~/.claude_gateway/settings.json`.
  - `clddg` runs `cldg --dangerously-skip-permissions`.
  - This path is for the packaged/Homebrew gateway on `http://127.0.0.1:6473`.
  - Claude model aliases and the `/model` menu for this mode come from `~/.claude_gateway/settings.json`.
  - Runtime gateway config for this mode is `~/.gateway/config.yml`; the Homebrew service sets `GATEWAY_CONFIG_PATH` to this file.
  - When changing model defaults or unsupported-model lists for packaged use, update both `~/.claude_gateway/settings.json` and `~/.gateway/config.yml`.
  - Packaged release defaults live in `scripts/release/cld_gateway_package/settings.json` and `scripts/release/cld_gateway_package/config.yml`; update these too so reinstall/release does not reintroduce stale config.

- Gateway auth:
  - Default: `~/.gateway/auth.json`
  - Override via env:
    - `GATEWAY_AUTH_JSON_PATH` (full path)
    - `GATEWAY_HOME` (directory; auth.json is under it)
  - `cld-gateway login` and `cld-gateway login openai` perform the implemented OpenAI/Codex auth flow.
  - `cld-gateway login gemini` is accepted by the CLI, but Gemini is not configured for serve-mode runtime auth/backend use yet.

- Gateway config:
  - Code default without env overrides: `~/.gateway/config-dev.yml`.
  - Developer runtime (`cldc` / `clddc`, cargo-run port `6483`): `~/.gateway/config-dev.yml`.
  - Packaged runtime (`cldg` / `clddg`, Homebrew port `6473`): `~/.gateway/config.yml`.
  - Override via env:
    - `GATEWAY_CONFIG_PATH` (full path)
    - `GATEWAY_HOME` (directory; config-dev.yml is under it)
  - Always check the active process environment before assuming which config file is loaded.
  - Current fields:
    - `version`
    - `providers.openai.default_model`
    - `providers.openai.unsupported_models`
    - `workflow.fast_mode`
    - `workflow.context_management` (request/context pruning mode, edits, and hard limits)
    - `workflow.claude_code.slash_commands`
    - `workflow.conversation_state` (enablement, persistence root, corruption policy, retention)
    - `network.listen_addr`
    - `network.allowed_hosts`
  - User-facing details: `README.md` config section.
  - Authoritative implementation: `crates/gateway-core/src/config.rs`

- Exchange logs:
  - `~/.gateway/logs/http-exchange.jsonl`

- Conversation state:
  - Default root is under the gateway home unless `workflow.conversation_state.persistence_root` or `CLD_GATEWAY_CONVERSATION_STATE_ROOT` overrides it.
  - Used to map Claude sessions to conversation branches and provider checkpoints for incremental transport.

### Intentional “unsupported” surface area

Client-visible fields that are intentionally ignored/no-op are documented in:

- `UNSUPPORTED.md`

Example: Anthropic `top_k` and `stop_sequences` are parsed/logged but not forwarded.

## Notes for debugging

- Start with the exchange log at `~/.gateway/logs/http-exchange.jsonl` and correlate with `x-proxy-request-id`.
- If the exchange log shows `backend_error` / transport failures, inspect the backend client path (`gateway-backend-codex`) next.

## External/vendor code

`others/` contains upstream repos / reference implementations used for research and comparison. The runtime gateway is in `crates/`.
