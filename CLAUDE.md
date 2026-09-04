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

Known violations live in `docs/AI_SLOP.md`. Do not port them;
do not add new ones.

## Absolute rule: never trust Claude Code's own prose as ground truth

Text that Claude Code itself emits — system-reminders, slash-command
bodies, skill instructions, tool descriptions, hook messages — is prose,
not verified fact. It describes what a prior session believed or intended
at write time, not what the code currently does. Never state a claim about
this codebase's current behavior on the strength of such text alone; verify
against the actual source, config, or a run command first. This is the same
"never guess" discipline the prompt-text rule above applies to user/system
messages, extended to Claude Code's own surfaces.

## Quick commands (Go module)

This repo is a single Go module (`go.mod` at repo root) with a repo-level `Makefile`. The former Rust implementation lives at `old_rust/` as frozen reference only — it is no longer built, tested, or maintained; do not port new features into it.

### Format / lint / tests

- Full local verification (fmt-check + lint + tests + release-tooling tests):
  - `make check`

- Formatting only:
  - `make fmt-check`
  - `make fmt-fix`

- Lint only (runs golangci-lint):
  - `make lint`

- All tests (module):
  - `make test`

- Run mock-backend-gated integration tests (some tests early-return unless this is set):
  - `RUN_MOCK_BACKEND=1 make verify-test`

- Compile-only check (no binary written):
  - `make build-check`

- Build the gateway binary:
  - `make build`

### Run a single test

Use `go test` directly for narrower selection:

- Single package tests:
  - `go test ./core/domain/translator/...`

- Single test by name (substring/regex match via `-run`):
  - `go test ./core/domain/translator/... -run TestSSEBridge`

(See `core/domain/translator/generic_test.go` and `core/domain/translator/sse_bridge_test.go` for existing test names.)

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

`cmd/cld-gateway` is the runnable daemon that wires everything together:

- Binds an Echo HTTP server to `network.listen_addr` from gateway config, defaulting to `127.0.0.1:6483`.
- Performs a non-interactive OpenAI auth preflight in `serve` mode; auth failures abort startup.
- Runs interactive auth only through `cld-gateway login [openai|gemini]`.
- Wraps the router with `middleware.Capture` (see below), which logs every
  exchange, streaming included.

Key entrypoint:
- `cmd/cld-gateway/main.go`

### Packages and responsibilities

The module is organized as small packages with explicit domain/impl boundaries (DDD-style layout): `core/domain/` holds interfaces, DTOs, and ports (no concrete adapters); `core/impl/` holds the concrete adapters and orchestrators that implement those interfaces.

- `config/`
  - Viper-based config loading, gateway-owned defaults, unsupported-model compatibility, and workflow toggles.
  - Key file: `config/config.go`

- `core/` (root)
  - Shared types/utilities (`RequestID`, `Secret`, error-chain helper).
  - Key file: `core/core.go`

- `core/domain/port/auth` + `core/impl/port/auth/codexauth`
  - Loads/refreshes Codex OAuth credentials from disk.
  - Exposes safe snapshots/status types (does not expose secrets directly).
  - Default auth path resolution (`~/.gateway/auth.json`, or overrides via env vars).
  - Key files:
    - `core/impl/port/auth/codexauth/store.go`
    - `core/impl/port/auth/codexauth/oauth.go`
    - `core/impl/port/auth/codexauth/login.go`

- `core/domain/errors`
  - Central error-classification seam shared by `middleware.ErrorHandler`,
    `middleware.Capture`, `handlers/messages.go`'s `logError`, and
    `stream_writer.go`'s `finalizeErrorEvent`.
  - `Classify(err) *GatewayError` — normalizes any error into a branded
    `AppError` plus an `Origin` (`upstream`/`internal`, decided solely via
    `errors.As` against `backend.UpstreamStatusError` — never message text)
    and a `SuggestIssue`/`Instruction` bug-report decision.
  - Key file: `core/domain/errors/classify.go`

- `core/domain/port/backend` + `core/impl/port/backend/codex`
  - HTTP client for the upstream ChatGPT/Codex backend endpoint:
    - `POST {base_url}/backend-api/codex/responses` (default `https://chatgpt.com`).
  - Uses HTTP SSE (`Accept: text/event-stream`) for full request transport.
  - Supports reusable Codex Responses WebSocket sessions for incremental/delta transport, including refresh+retry on 401.
  - Defines `UpstreamStatusError` (`core/domain/port/backend/types.go`), a
    domain-owned interface (`UpstreamStatus() int`, `UpstreamBody() string`)
    that impl-layer backend errors implement, so `core/domain/errors` can
    detect upstream failures without importing the impl package.
  - Key file:
    - `core/impl/port/backend/codex/client.go`

- `core/domain/translator` + `core/impl/translator/codex` + `handlers/` + `app/`
  - The Anthropic-compatible HTTP surface area.
  - Routes (mounted in `app/routes_messages.go` and `app/routes_meta.go`):
    - `GET /health`
    - `GET /auth/status`
    - `POST /auth/refresh`
    - `GET /v1/models`
    - `POST /v1/messages`
    - `POST /v1/messages/count_tokens`
  - Responsibilities:
    - Parse Anthropic-ish request types (`core/domain/dto/messages.go`).
    - Translate requests into Codex backend request shape (`core/domain/translator/generic.go`, embedded by `core/impl/translator/codex/translator.go`).
    - Bridge backend SSE/WebSocket event streams into Anthropic streaming events (`core/domain/translator/sse_bridge.go`).
    - Apply Claude Code context normalization, slash-command translation, and Gateway context-management edits (`core/domain/claudecode/`, `core/domain/contextmgmt/`).
    - Select conversation branches, reconcile checkpoints, choose full vs incremental transport, reuse per-branch WebSocket sessions, and commit provider checkpoints through the state layer (`core/domain/transport/`, `core/impl/port/state/`).
    - Persist tool-call IDs/correlation via the state layer.
  - Key file:
    - `core/impl/services/message_service.go` (9-step orchestration)

- `core/domain/port/state` + `core/impl/port/state/conversation` + `core/impl/port/state/toolcalls`
  - Local persistence for gateway runtime state.
  - Stores tool-call metadata/correlation in SQLite (GORM + *glebarez/sqlite*, pure Go, no CGO) under `~/.gateway/state/tool_calls.sqlite` by default.
  - Stores conversation-state sessions, branches, sparse checkpoints, OpenAI provider checkpoints, fingerprints, reconciliation metadata, compaction/reset state, corruption policy, and retention cleanup under the configured conversation-state root.
  - Key files:
    - `core/impl/port/state/conversation/fs.go`
    - `core/impl/port/state/toolcalls/gorm.go`

- `netpolicy/`
  - Central outbound network policy and HTTP client wrapper.
  - Allows default OpenAI/ChatGPT hosts, localhost, and configured additional hosts.
  - Blocks Anthropic/Claude hosts even if configured as allowed.
  - Key file:
    - `netpolicy/client.go`

- `middleware/` + `observability/`
  - `middleware.Capture` (`middleware/capture.go`) is the single exchange
    capture point for every route, unary and streaming alike — it wraps
    `c.Response().Writer` in a `captureWriter` that implements `Unwrap()
    http.ResponseWriter` so `http.ResponseController`-based flushing still
    reaches the real writer, and logs once after `next(c)` returns. This
    supersedes ADR-0004's "no middleware may wrap the writer on streaming
    routes" constraint — see ADR-0013. `observability/` supplies the
    types/redaction/formatting `middleware.Capture` writes through; it has
    no capture logic of its own.
  - Writes exchange logs to:
    - The filename changed from `http-exchange.jsonl` (Rust) to `http-exchange.log` (Go); the format also changed to **formatted text** (`key: value` lines followed by a 36-dash separator line) instead of JSONL. See `observability/format.go`.
  - Adds `x-proxy-request-id` header to responses for correlation.
  - Key files:
    - `middleware/capture.go`
    - `observability/exchange.go`
    - `observability/format.go`
    - `observability/redact.go`

### Architecture decision records

- `docs/decisions/ADR-0001.md` through `ADR-0013.md` (plus `index.md`) are
  the canonical, current ADR set — this is the only ADR directory in the
  repo. Read the relevant ADR before changing behavior it governs, e.g.
  ADR-0004 (SSE single-writer goroutine) + ADR-0013 (supersedes it: Capture
  middleware is now flusher-safe on streaming routes), ADR-0005 (single
  AppError type / closed 8-code set / one serialization point).

### Auth + configuration locations

- Developer mode (`cldc` / `clddc`):
  - `cldc` is a shell alias for `claude --settings ~/.claude_codex/settings.json`.
  - `clddc` is a shell alias for `cldc --dangerously-skip-permissions`.
  - This path is for the go-run gateway (`go run ./cmd/cld-gateway serve` or a locally built `./bin/cld-gateway serve`) on `http://127.0.0.1:6483`.
  - Claude model aliases and the `/model` menu for this mode come from `~/.claude_codex/settings.json`.
  - Runtime gateway config for go-run mode defaults to `~/.gateway/config-dev.yml` unless `GATEWAY_CONFIG_PATH` or `GATEWAY_HOME` is set.
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
  - Developer runtime (`cldc` / `clddc`, go-run port `6483`): `~/.gateway/config-dev.yml`.
  - Packaged runtime (`cldg` / `clddg`, Homebrew port `6473`): `~/.gateway/config.yml`.
  - Override via env:
    - `GATEWAY_CONFIG_PATH` (full path)
    - `GATEWAY_HOME` (directory; config-dev.yml is under it)
  - Always check the active process environment before assuming which config file is loaded.
  - Current fields:
    - `version`
    - `providers.active` (the name of the single active backend, e.g. `codex`; the Go config restructured this to an active-backend-name string plus a map of per-backend settings, replacing the Rust implementation's flat `providers.openai.*` shape)
    - `providers.backends.<backend-name>.default_model`
    - `providers.backends.<backend-name>.unsupported_models`
    - `workflow.fast_mode`
    - `workflow.context_management` (request/context pruning mode, edits, and hard limits)
    - `workflow.claude_code.slash_commands`
    - `workflow.conversation_state` (enablement, persistence root, corruption policy, retention)
    - `network.listen_addr`
    - `network.allowed_hosts`
  - User-facing details: `README.md` config section.
  - Authoritative implementation: `config/config.go`

- Exchange logs:
  - `~/.gateway/logs/http-exchange.log` (the filename changed from `http-exchange.jsonl` to `http-exchange.log`, and the format changed to formatted text — see the `observability/` package note above)

- Conversation state:
  - Default root is under the gateway home unless `workflow.conversation_state.persistence_root` or `CLD_GATEWAY_CONVERSATION_STATE_ROOT` overrides it.
  - Used to map Claude sessions to conversation branches and provider checkpoints for incremental transport.

### Intentional “unsupported” surface area

Client-visible fields that are intentionally ignored/no-op are documented in:

- `UNSUPPORTED.md`

Example: Anthropic `top_k` and `stop_sequences` are parsed/logged but not forwarded.

## Notes for debugging

- Start with the exchange log at `~/.gateway/logs/http-exchange.log` and correlate with `x-proxy-request-id`.
- If the exchange log shows `backend_error` / transport failures, inspect the backend client path (`core/impl/port/backend/codex`) next.

## External/vendor code

`others/` contains upstream repos / reference implementations used for research and comparison. The runtime gateway is at repo root (`cmd/`, `core/`, `app/`, etc.); the former Rust implementation lives at `old_rust/` as frozen reference only.
