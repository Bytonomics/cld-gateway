---
type: decision
title: "ADR-0009: Library selections"
status: stable
tags:
  - adr
  - dependencies
generated:
  by: claude-sonnet-5
  at: 2026-09-03T00:00:00Z
---

# ADR-0009: Library selections

| Section | What it covers |
|---------|----------------|
| [Status](#status) | Where this decision stands |
| [Date](#date) | When it was decided |
| [Context](#context) | The problem, requirements, and constraints |
| [Options Considered](#options-considered) | The alternatives with pros and cons |
| [Decision](#decision) | What was chosen and why |
| [Consequences](#consequences) | Positive, negative, and mitigations |
| [References](#references) | Sources and related decisions |

## Status

Accepted

## Date

2026-08-22

## Context

Every subsystem in the Rust implementation used a specific crate — a WebSocket client, an
ORM, a config loader, a TUI framework, a keyring integration. Porting to Go means picking a
Go library for each, and the right pick isn't always "the Go library that looks most like
the Rust one" — sometimes it's a different shape entirely, and sometimes (keyring) it's
nothing at all.

## Options Considered

### Option 1: Match each Rust crate 1:1 with its closest-named Go equivalent

**Pros:**

- Minimizes decision-making per concern — pick whatever looks most similar to what Rust
  used.

**Cons:**

- Would have carried a keyring dependency across the port for a code path verified to be
  dead (credentials are file-based on both sides), adding OS-specific complexity nothing
  exercises.
- Ignores that some concerns (TUI picker) have no comparable Rust-side dependency to match
  against in the first place.

### Option 2: Pick the best-fit Go library per concern, verified against actual need

**Pros:**

- Every pick either satisfies a hard constraint (C-dependency-free builds for WebSocket and
  the SQLite driver) or reuses something already proven in `smritea-cloud`.
- Drops dependencies verified to be unnecessary (keyring) instead of carrying them "for
  parity."

**Cons:**

- Requires actually verifying each concern's real requirement rather than pattern-matching
  off the Rust crate list, which takes more upfront investigation per choice.

## Decision

| Concern | Choice | Why |
|---|---|---|
| HTTP framework | `echo` v4 | See [ADR-0003](ADR-0003-echo-pedantigo.md). |
| Validation + binder | `pedantigo` v2 / `pedantigoecho` v2 | See [ADR-0003](ADR-0003-echo-pedantigo.md). |
| WebSocket | *coder/websocket* (formerly *nhooyr/websocket*) | Actively maintained, minimal API, no C dependency — keeps `CGO_ENABLED=0` builds viable. |
| ORM / storage | GORM + *glebarez/sqlite* | Pure-Go SQLite driver (no cgo), so the daemon binary stays statically buildable; GORM chosen for the same reason `smritea-cloud` already uses it — proven at comparable scope, and a Postgres swap stays possible later without changing the query layer. |
| Config | *spf13/viper* | Matches the `smritea-cloud` shared config pattern already in use elsewhere; env-var override support (`GATEWAY_*`) comes built in rather than hand-rolled. |
| TUI login picker | `bubbletea` + `lipgloss` | The standard idiomatic choice for a terminal picker UI in Go; no comparable Rust-side dependency to match against. |
| Logging | *log/slog* (standard library) | No third-party logging dependency needed — structured logging with request-ID fields is a stdlib feature as of the Go versions this module targets. |
| Errors | `AppError` + sentinel errors + `%w` wrapping | Standard Go error-handling idiom; see [ADR-0005](ADR-0005-apperror-anthropic-error-shape.md) for how this surfaces at the HTTP boundary. |
| Keyring | **Dropped** | Verified: the Rust keyring integration was not the actual auth storage path — credentials are file-based (`~/.gateway/auth.json`) on both sides. Carrying a keyring dependency across the port would have added OS-specific complexity for a code path nothing exercises. |
| UUID | *google/uuid* | Standard, widely used, no reason to deviate. |
| SSE | Hand-rolled writer (~80 lines) | The wire format is simple enough that a dependency would cost more than it saves, and hand-rolling it keeps the single-writer model (see [ADR-0004](ADR-0004-sse-single-writer-goroutine.md)) fully under our control rather than behind a library's own buffering assumptions. |

## Consequences

### Positive

- Every pick above either has C-dependency-free builds as a hard constraint (WebSocket,
  SQLite driver) or reuses something already proven in `smritea-cloud` — this port isn't
  the place to introduce a novel dependency choice without a specific reason.

### Negative

- Dropping the keyring is a real behavior narrowing versus a keyring-capable Rust build.

### Mitigation

- Verified as dead code before dropping, not an assumption — see the codex-auth store
  implementation for the actual read/write path.

## References

- [ADR-0003: Echo + pedantigoecho + pedantigo v2](ADR-0003-echo-pedantigo.md)
- [ADR-0004: SSE single-writer goroutine + Option C post-stream logging](ADR-0004-sse-single-writer-goroutine.md)
- [ADR-0005: AppError → Anthropic error shape](ADR-0005-apperror-anthropic-error-shape.md)
- `core/impl/port/auth/codexauth/`
