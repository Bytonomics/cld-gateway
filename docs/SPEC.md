# SPEC: golang_port (consolidated, single file)

Version: 1.0 — consolidates all grill-me interview rounds (2026-08-22).
Canonical in-repo spec; GitHub issue #3 mirrors this text.

## Problem Statement

The gateway is a single-user local Rust daemon (~17k lines, 8 crates) whose
`gateway-http-anthropic/src/lib.rs` is a 10k-line god object owning routing,
translation, transport selection, and commits. The owner wants a maintainable
Go rewrite that preserves every runtime behavior and the Homebrew install
contract, re-architects the outbound side as pluggable backends (1-to-n),
and permanently excludes the Codex-generated prompt-text heuristics
(`AI_SLOP.md`).

## Solution

A Go port in `golang_port/` following the smritea-cloud DDD layout — thin
`cmd/` main, `app/` with a Providers struct and manual constructor DI,
`core/domain` (service interfaces + ports) and `core/impl` (implementations),
thin `handlers/`, `middleware/` — built on Echo v4 with the pedantigoecho
binder and pedantigo v2. The outbound side is a Backend port with one Codex
implementation and extend-via-composition translators. SSE streaming runs on
a single-writer goroutine over an event channel; exchange logging happens
post-stream in formatted text entries.

## User Stories

1. As the operator, I want the same Homebrew tap formula to install the Go
   binary, so my install/upgrade flow does not change.
2. As the operator, I want `cld-gateway serve` / `login [vendor]` preserved,
   so my muscle memory and service definitions keep working.
3. As Claude Code (the client), I want `POST /v1/messages` to behave
   identically to the Rust gateway, so sessions work without client changes.
4. As Claude Code, I want SSE events to arrive incrementally without
   buffering, so responses render live.
5. As Claude Code, I want errors in the Anthropic error shape, so my SDK
   parses them correctly.
6. As the operator, I want exchange logs as formatted text entries (key:
   value lines + dashed separator), so I can read them with plain eyes.
7. As the operator, I want conversation-state persistence (branches,
   checkpoints, ledger) preserved on disk, so existing sessions survive the
   rewrite.
8. As the operator, I want the network policy to block Anthropic/Claude
   hosts even via redirects, so the proxy invariant holds.
9. As the operator, I want main-turn lease semantics preserved, so aborted
   clients never commit visible turns.
10. As the operator, I want WebSocket continuation only when the live chain
    matches the stored checkpoint association, so incremental transport
    stays correct.
11. As a future backend author, I want a Backend port abstraction, so I can
    add a provider without touching core.
12. As a future backend author, I want a GenericBackendTranslator base to
    compose, so a new backend reuses shared translation and overrides the
    rest.
13. As the operator, I want pedantigo validation with field-path errors, so
    bad requests fail loudly and early.
14. As the operator, I want AI_SLOP.md heuristics excluded, so
    classification uses structural signals only.
15. As the operator, I want model resolution and unsupported-model fallback
    preserved, so model routing matches current behavior.
16. As the operator, I want auth preflight to re-run login on refresh
    failure at serve startup, so the server never serves without valid auth.
17. As the operator, I want the SQLite tool-call store preserved, so
    tool-call correlation survives the rewrite.
18. As the operator, I want the model catalog from local Claude settings
    JSON, so the /model menu keeps working.
19. As the operator, I want `cld-gateway-sh setup` to modify config
    (including active backend), so I can reconfigure without editing YAML.
20. As a maintainer, I want no file to own routing, translation, transport
    selection, and commits simultaneously, so the codebase stays navigable.

## Implementation Decisions

### Scope
- 1-to-n: Claude Code is the ONLY harness; backends are the extension axis;
  one active backend at a time; no harness port; no neutral IR.

### Layout (approved)
- Single Go module `github.com/Bytonomics/cld-gateway`; entrypoint
  `cmd/cld-gateway`; smritea-cloud layout: app/ (Providers, routes_*.go,
  manual DI — no fx/wire), core/domain + core/impl mirror, handlers/,
  middleware/, observability/. Compile-time interface asserts.

### Libraries (approved — replaces candidate list)
- echo v4; pedantigo v2 (`validate` tag) for validation + LLM schema;
  pedantigoecho v2 binder on e.Binder; coder/websocket; GORM +
  glebarez/sqlite (pure Go; swap to gorm.io/driver/postgres later without
  model changes); spf13/viper for config (smritea shared/config pattern);
  bubbletea TUI for interactive login; log/slog; google/uuid; hand-rolled
  SSE writer; keyring DROPPED (auth is file-based). CGO_ENABLED=0 required.

### DDD decomposition
- Per-use-case services in domain/services with orchestrators in
  impl/services; no god orchestrator.
- Backend port: SendUnary/SendStream/Capabilities/EvictSession; one Codex
  impl (SSE + pooled WS, chain IDs, 401 refresh-retry-once).
- Translator: BackendTranslator interface; GenericBackendTranslator shared
  base; `type OpenAITranslator struct { *GenericBackendTranslator }`;
  both satisfy the interface (extend-via-composition).
- Conversation state stays core, NOT a pluggable port; common format kept
  exactly (branches, sparse checkpoints, JSONL ledger, retention,
  fail-closed corruption).
- Routing: one active backend named in config; `cld-gateway-sh setup`
  gains options to modify config.

### Concurrency (core feature — port verbatim)
- Per-session mutex registry + main-turn lease state machine:
  in_flight, completed_committed, client_aborted_before_first_event,
  client_aborted_after_visible_output, backend_failed_before_commit,
  commit_suppressed_after_abort. Only in_flight allows commit.

### SSE + logging (settled)
- Single writer goroutine owns the response writer; channel of events;
  write + Flush per event; headers before first write; no middleware wraps
  the writer on streaming paths; streaming must not be compromised.
- Option C logging: post-stream log write from accumulated events; unary
  keeps capture middleware; cleanup via close(ch) + ctx.Done(); no writes
  after handler return.
- Log format: key: value lines per entry, dashed separator line ends each
  entry, next entry on next row.

### Errors
- AppError redesigned to serialize directly as the Anthropic error shape
  `{"type":"error","error":{"type":code,"message":msg}}`; central Echo
  HTTPErrorHandler; pedantigo field paths map into invalid_request_error.

### Config
- viper; providers as a map keyed by backend name with `active: true` on
  one; env overrides GATEWAY_CONFIG_PATH / GATEWAY_HOME; packaged vs dev
  file locations preserved.

### Invariants (all preserved)
- Anthropic-host denylist incl. redirects; checkpoint-gated WS reuse; lease
  commit gating; fail-closed corruption default; local-settings model
  catalog; auth preflight re-login; unsupported-model fallback; ~/.gateway
  file locations; listen addresses (6483 dev / 6473 packaged).

### Release
- Same formula contract and Python packager; build swaps cargo→go; targets
  remapped to Go OS/arch; Rust kept until parity + daily-driver cutover,
  then one-commit delete; rollback = revert release tag.

## Testing Decisions

- Good tests assert external behavior only: request in, HTTP/SSE out, disk
  state after.
- Seam: the HTTP boundary (highest existing seam), matching Rust contract
  tests.
- Parity gate: every Rust test reviewed and labeled CONTRACT / FAKE /
  SLOP-ADJACENT before porting (parked task: test-audit-and-migration-plan.md).
- CONTRACT tests port to Go per package with mocked ports (testify mocks
  next to impl, smritea style); golden-file tests for translation and SSE
  bridge parity.
- Streaming tests must assert bytes reach the client before response
  completion (flush invariant).

## Out of Scope

- Removing the four AI_SLOP heuristics from Rust (documented; separate task).
- Porting others/ vendored code.
- Gemini backend implementation (login accepted; runtime not configured —
  same as Rust).
- Changing the Homebrew formula structure or packager beyond build swap.
- Classification replacement signals (parked:
  classification-signal-redesign.md).
- Second backend implementation (design supports it; none is built).

## Further Notes

- Parked docs live in golang_port/docs/: classification-signal-redesign.md,
  test-audit-and-migration-plan.md, AI_SLOP.md.
- ARCHITECTURE_v2.md (this folder) is the detailed package design matching
  this spec.
- The owner explicitly distrusts the slop inventory: the re-audit must both
  find missed prompt-text decisions and re-verify the four known ones before
  replacement signals are designed.
