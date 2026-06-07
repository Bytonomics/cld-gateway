# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

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

## Codebase structure (big picture)

### Runtime topology

`gatewayd` is the runnable daemon that wires everything together:

- Starts an Axum HTTP server on `127.0.0.1:8080`.
- Ensures auth exists (interactive login if needed).
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
  - Uses SSE (`Accept: text/event-stream`) and supports refresh+retry on 401.
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
    - Bridge backend SSE event stream into Anthropic streaming events (`sse_bridge.rs`).
    - Persist tool-call IDs/correlation via `gateway-state`.
  - Key file:
    - `crates/gateway-http-anthropic/src/lib.rs`

- `crates/gateway-state`
  - Minimal local persistence (SQLite via `rusqlite`) for correlation/IDs.
  - Currently used for tool call storage (`ToolCallStore`) under `~/.gateway/state/tool_calls.sqlite`.
  - Key file:
    - `crates/gateway-state/src/lib.rs`

- `crates/gateway-observability`
  - Request/response capture middleware (no business logic).
  - Writes exchange logs to:
    - `~/.gateway/logs/http-exchange.jsonl`
  - Adds `x-proxy-request-id` header to responses for correlation.
  - Key files:
    - `crates/gateway-observability/src/middleware.rs`
    - `crates/gateway-observability/src/paths.rs`

### Auth + configuration locations

- Gateway auth:
  - Default: `~/.gateway/auth.json`
  - Override via env:
    - `GATEWAY_AUTH_JSON_PATH` (full path)
    - `GATEWAY_HOME` (directory; auth.json is under it)

- Gateway config:
  - Default: `~/.gateway/config-dev.yml`
  - Override via env:
    - `GATEWAY_CONFIG_PATH` (full path)
    - `GATEWAY_HOME` (directory; config.yml is under it)
  - Current fields:
    - `providers.openai.default_model`
    - `providers.openai.unsupported_models`
    - `workflow.fast_mode`
  - Details: `docs/gateway_config.md`

- Exchange logs:
  - `~/.gateway/logs/http-exchange.jsonl`

### Intentional “unsupported” surface area

Client-visible fields that are intentionally ignored/no-op are documented in:

- `UNSUPPORTED.md`

Example: Anthropic `top_k` and `stop_sequences` are parsed/logged but not forwarded.

## Notes for debugging

- Start with the exchange log at `~/.gateway/logs/http-exchange.jsonl` and correlate with `x-proxy-request-id`.
- If the exchange log shows `backend_error` / transport failures, inspect the backend client path (`gateway-backend-codex`) next.

## External/vendor code

`others/` contains upstream repos / reference implementations used for research and comparison. The runtime gateway is in `crates/`.
