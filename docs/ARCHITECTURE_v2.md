---
type: explanation
title: "Gateway Architecture (Detailed Package Design)"
status: stable
tags:
  - architecture
  - go-port
  - design
stale_after: 2027-05-01
generated:
  by: claude-sonnet-5
  at: 2026-09-03T00:00:00Z
---

# Gateway Architecture (Detailed Package Design)

| Section | What it covers |
|---------|----------------|
| [Scope: 1-to-n](#scope-1-to-n) | Claude Code as the only inbound harness; backends as the extension axis |
| [Layout (single Go module)](#layout-smritea-cloud-style-single-go-module) | Package tree and its responsibilities |
| [Ports (core/domain/port)](#ports-coredomainport) | Backend, translator, auth, and state interfaces |
| [Request flow (POST /v1/messages)](#request-flow-post-v1messages) | The 9-step orchestration |
| [Concurrency (CORE FEATURE — port verbatim)](#concurrency-core-feature--port-verbatim) | The main-turn lease state machine |
| [SSE + logging (settled)](#sse--logging-settled) | Single-writer goroutine and Option C logging |
| [Log format (user-specified)](#log-format-user-specified) | The formatted-text exchange log entry shape |
| [Error model](#error-model) | AppError to Anthropic error-shape serialization |
| [Config (viper)](#config-viper) | Provider map shape and env overrides |
| [Behavioral invariants (from Rust — preserve ALL)](#behavioral-invariants-from-rust--preserve-all) | Runtime behaviors carried over from the Rust implementation |
| [Approved libraries](#approved-libraries) | The library choices and why |
| [Homebrew / release](#homebrew--release) | Packaging and rollback contract |
| [Related documents](#related-documents) | Where the distilled version and the file-level reference live |

This is the detailed package design for the gateway's Go implementation, matching the current
codebase's `core/domain` / `core/impl` split. A shorter distillation lives at
`docs/explanation/architecture.md`; the exhaustive file/interface listing is
`docs/FILEMAP.md`. The Rust implementation this design replaced is frozen for reference at
`old_rust/`; slop exclusions carried forward from it are documented in `docs/AI_SLOP.md`.

## Scope: 1-to-n

Claude Code is the ONLY inbound harness (its translation is
Claude-Code-specific). The extension axis is BACKENDS: many outbound
providers, one active at a time. No harness port. No neutral IR.

## Layout (smritea-cloud style, single Go module)

Module github.com/Bytonomics/cld-gateway (see `go.mod`). One module. No workspace.

```
cmd/cld-gateway/main.go     thin: argv (serve | login [vendor]), calls app
app/                        Providers struct (manual constructor DI),
                            initialize.go, router.go, routes_*.go
core/domain/                services/   use-case interfaces + DTOs
                            port/       backend/, auth/, state/ adapters
                            translator/ BackendTranslator iface + generic base
core/impl/                  services/   orchestrators
                            port/backend/codex/
                            port/auth/
                            translator/codex/
handlers/                   thin Echo handlers, constructor-injected
middleware/                 request-id, recovery, unary capture
observability/              exchange logs, redaction
```

Compile-time asserts everywhere: `var _ services.MessageService = (*MessageService)(nil)`.

## Ports (core/domain/port)

### backend
```go
type Backend interface {
    SendUnary(ctx, *BackendRequest) (*BackendResponse, error)
    SendStream(ctx, *BackendRequest) (<-chan BackendEvent, error)
    Capabilities() Capabilities // e.g. WebSocketDelta, ServerSideState
    EvictSession(sessionKey SessionKey)
}
```
One impl today: `core/impl/port/backend/codex` — POST
`https://chatgpt.com/backend-api/codex/responses` (SSE), pooled WebSocket sessions
per SessionKey, chain IDs, 401 refresh-retry-once. A second backend later
adds one package, zero core edits.

### translator (extend-via-composition)
```go
type BackendTranslator interface {
    TranslateRequest(ctx, *AnthropicMessagesRequest) (*BackendRequest, error)
    TranslateResponseEvent(ev BackendEvent) ([]AnthropicSSEEvent, error)
}

type GenericBackendTranslator struct { /* shared: system->instructions,
    message shaping, tool schema gating, output config mapping */ }

type OpenAITranslator struct { *GenericBackendTranslator }
// embeds pointer; overrides Codex-specific parts; BOTH satisfy
// BackendTranslator
```

### auth, state
- `auth.Provider`: AccessToken/AccountID/Refresh. Codex impl reads/writes
  `~/.gateway/auth.json` (env overrides per Rust paths.rs), PKCE login flow.
- State stays core (NOT a pluggable port): `ConversationRepo`, `ToolCallRepo`
  (GORM), `CheckpointRepo`. The common conversation-state format is kept
  exactly — branches, sparse checkpoints, JSONL ledger, retention,
  fail-closed corruption policy.

## Request flow (POST /v1/messages)

handlers/ binds+validates (pedantigoecho) →
`MessageService.Handle(ctx, req)`:
1. classify kind (PARKED — see classification-signal-redesign.md; structural
   signals only, no prompt text)
2. normalize Claude Code context (command envelopes, slash-command promotion,
   directive injection — all keep; injection, not classification)
3. context management (edits, hard limits)
4. resolve model (active backend default/unsupported policy)
5. branch selection + transport identity (session, branch, kind, model
   fingerprint, effort)
6. translate via BackendTranslator for active backend
7. transport select: WS delta if live chain matches stored checkpoint
   association, else full SSE; lease acquire
8. single writer goroutine consumes event channel → write + Flush per event
9. lease commit gate → state commits (turn/offshoot checkpoints, ledger,
   tool calls) → Option C logging

## Concurrency (CORE FEATURE — port verbatim)

Per-session mutex registry (session_lock_registry parity) + main-turn lease
state machine, states:
in_flight → completed_committed
          → client_aborted_before_first_event
          → client_aborted_after_visible_output
          → backend_failed_before_commit
          → commit_suppressed_after_abort
Only in_flight.allows_commit() == true. This prevents aborted clients from
committing visible turns. Do not redesign.

## SSE + logging (settled)

- ONE goroutine owns the response writer; reads event channel; writes SSE
  bytes; Flush() per event. Headers before first write.
- NO middleware wraps the response writer on streaming paths (kills the
  Flusher-forwarding bug class).
- Streaming must not be compromised: bytes reach client per event.
- Logging = Option C: post-stream. Writer goroutine hands accumulated events
  to logger when the stream closes; log write happens after; unary keeps
  capture middleware. Cleanup: close(ch), watch ctx.Done(); no writes after
  handler return.

## Log format (user-specified)

Formatted text, one entry per exchange:
```
key1: value1
key2: value2
------------------------------------
```
Dashed separator line ends every entry; next entry starts on the next row.

## Error model

AppError redesigned to serialize as the Anthropic error shape:
`{"type":"error","error":{"type":"<code>","message":"..."}}` via a central
Echo HTTPErrorHandler. Pedantigo field-path validation errors map to
`invalid_request_error` with paths in the message.

## Config (viper)

`~/.gateway/config-dev.yml` (cargo-run) / `~/.gateway/config.yml` (packaged),
env `GATEWAY_CONFIG_PATH`/`GATEWAY_HOME`. Providers structured with explicit
`active` backend name and `backends` map:
```yaml
providers:
  active: codex
  backends:
    codex:              # backend name
      default_model: gpt-5.6-sol
      unsupported_models: [...]
```
`cld-gateway-sh setup` gains options to modify config (choose active
backend, etc.).

## Behavioral invariants (from Rust — preserve ALL)

1. Network policy blocks anthropic.com/claude.ai even via redirects.
2. previous_response_id reuse only when live WS chain matches stored
   checkpoint association (checkpoint_key = identity + response_id).
3. Lease gates commits; aborted clients never commit visible turns.
4. Corruption policy default fail-closed; quarantine-and-reset opt-in.
5. Model catalog from local Claude settings JSON (~/.claude_gateway/...).
6. Auth preflight re-runs login on refresh failure at serve startup.
7. Unsupported-model fallback to active backend default_model.
8. Logs: ~/.gateway/logs/ (exchange, transport-decisions.jsonl).
9. SQLite tool calls: ~/.gateway/state/tool_calls.sqlite.
10. Conversation-state root: ~/.gateway/sessions/claudecode.
11. Listen 127.0.0.1:6483 (dev) / 6473 (packaged) per config.

## Approved libraries

| Concern | Choice |
|---|---|
| HTTP framework | echo v4 |
| Validation + schema | pedantigo v2 (`validate` tag) |
| Binder | pedantigoecho v2 (`e.Binder = pedantigoecho.NewBinder()`) |
| WebSocket | coder/websocket (ex-nhooyr) |
| ORM/storage | GORM + glebarez/sqlite (pure Go; PG swap later) |
| Config | spf13/viper (smritea shared/config pattern) |
| TUI login | bubbletea (+ lipgloss) |
| Logging | log/slog (stdlib) |
| Errors | AppError→Anthropic shape; sentinels + %w |
| Keyring | DROPPED (auth is file-based; verified) |
| UUID | google/uuid |
| SSE | hand-rolled writer (~80 lines) |

Struct registration is mandatory:
`var _ = validator.Register(validator.New[T]())` per request struct (panics
otherwise). GET/DELETE query structs validated explicitly after c.Bind
(binder covers POST/PUT/PATCH bodies only).

## Homebrew / release

Same formula contract: bin/cld-gateway, cld-gateway-sh (setup gains config
options), cldg, clddg, config.yml, settings.json, commands, post_install.
Python packager retained; build step = `go build` with CGO_ENABLED=0; target
triples remapped to Go OS/arch pairs. Rust stays until parity + daily-driver
cutover, then one-commit delete; rollback = revert release tag.

## Related documents

- `docs/explanation/architecture.md` — the shorter, contributor-facing distillation of this
  document.
- `docs/FILEMAP.md` — the exhaustive file/interface listing this design maps to.
- `docs/AI_SLOP.md` — the prompt-text-dependency exclusions carried forward from Rust.
- `docs/classification-signal-redesign.md` — the parked classification-signal re-audit.
- `docs/test-audit-and-migration-plan.md` — the parked Rust test audit and migration matrix.
