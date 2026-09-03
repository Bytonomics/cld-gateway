---
type: how-to
title: "Extending: Adding a Backend"
status: stable
tags:
  - backends
  - extending
stale_after: 2027-01-01
generated:
  by: claude-sonnet-5
  at: 2026-09-03T00:00:00Z
---

# Extending: adding a backend

| Section | What it covers |
|---------|----------------|
| [1. Implement `port.Backend`](#1-implement-portbackend) | The interface a new backend package must satisfy |
| [2. Report capabilities honestly](#2-report-capabilities-honestly) | `WebSocketDelta` and `ServerSideState` |
| [3. Write a translator](#3-write-a-translator) | Pointer to the translator contract |
| [4. Add the config entry](#4-add-the-config-entry) | Wiring the new backend into config |
| [5. Auth, if the backend needs its own](#5-auth-if-the-backend-needs-its-own) | Implementing `port/auth.Provider` |
| [Testing a new backend](#testing-a-new-backend) | The recommended test seam |

The gateway routes to exactly one active backend at a time (see the
user-facing [Backends](../reference/configuration/backends.md) page for the
config-level model). This page is about the code side: what it takes to
add a *new* backend implementation for that config to select.

Per [ADR-0006](../decisions/ADR-0006-backend-port-composition-translators.md),
adding a backend touches only new files plus one config entry — no edits
to `core/domain`.

## 1. Implement `backend.Backend`

Add a package under `core/impl/port/backend/<name>/` implementing
`core/domain/port/backend/backend.go#Backend`:

```go
type Backend interface {
    SendUnary(ctx context.Context, req *Request) (*Response, error)
    SendStream(ctx context.Context, req *Request) (<-chan Event, error)
    Capabilities() Capabilities
    EvictSession(key SessionKey)
    HasLiveSession(key SessionKey) bool
    LiveChainID(key SessionKey) (ChainID, bool)
}
```

Use the existing `core/impl/port/backend/codex` package as the reference
implementation — it covers the shape of a real backend adapter: HTTP
client construction through `netpolicy` (never a bare `http.Client`, so
the outbound host allowlist in [Security](../explanation/security.md)
is enforced), SSE decoding for `SendStream`, and (where the backend
supports it) pooled WebSocket sessions keyed by `SessionKey`.

Add a compile-time assertion so a signature drift fails the build:

```go
var _ backend.Backend = (*Client)(nil)
```

## 2. Report capabilities honestly

`Capabilities()` tells core orchestration what this backend can do:

- `WebSocketDelta` — whether the backend supports resuming a live session
  incrementally (an existing backend chain matching a stored checkpoint).
  If `false`, every turn goes through full SSE — correct, just less
  efficient, and nothing else needs to change to support that.
- `ServerSideState` — whether the backend itself tracks conversation state
  server-side (relevant to how aggressively the gateway needs to resend
  context). If `false`, the gateway's own conversation state
  ([ADR-0007](../decisions/ADR-0007-conversation-state-core-not-pluggable.md))
  is the only source of truth and each
  request is built with full context per the backend's needs.

Under-reporting a capability just costs efficiency (more full resends than
necessary). Over-reporting one that isn't actually implemented will
surface as a runtime error the first time core tries to use it — get this
right rather than optimistic.

## 3. Write a translator

See [Extending: translators](extending-translators.md) for the full contract. In
short: embed `*translator.GenericBackendTranslator` and override only
what's specific to this backend's request/response shape.

## 4. Add the config entry

Add an entry under `providers` in the config schema
(`config/config.go`) and document it in
[Configuration: Backends](../reference/configuration/backends.md) — that
page is user-facing, so keep the documentation change there scoped to
what a user sets, not implementation detail.

## 5. Auth, if the backend needs its own

If the new backend has its own credential flow, implement
`core/domain/port/auth/auth.go#Provider` (`AccessToken`, `AccountID`,
`RefreshAndPersist`, `Status`, `Logout`) the same way
`core/impl/port/auth/codexauth` does, and wire the new provider into
`cld-gateway login <name>`.

## Testing a new backend

Follow the seam described in [Testing](testing.md) — fake the
`Backend` interface at the HTTP boundary for message-service-level tests,
and write focused tests against the real HTTP/WebSocket client separately
using `wiremock`-style fixtures rather than a live upstream.
