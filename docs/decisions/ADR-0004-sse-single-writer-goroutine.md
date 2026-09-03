---
type: decision
title: "ADR-0004: SSE single-writer goroutine + Option C post-stream logging"
status: superseded
tags:
  - adr
  - streaming
  - superseded
generated:
  by: claude-sonnet-5
  at: 2026-09-03T00:00:00Z
---

> **SUPERSEDED (2026-09-04) by [ADR-0013](ADR-0013-flusher-safe-capture-middleware-on-streaming.md).**
> The "no middleware wraps the response writer on streaming routes" constraint
> below (Decision, first bullet) does not hold as stated: it was written
> assuming `http.Flusher` could only be forwarded through a writer wrapper by
> implementing it directly. Go 1.20's `http.ResponseController` and its
> `Unwrap() http.ResponseWriter` convention — which `echo.Response.Flush()`
> itself already uses internally, verified against the installed Echo module
> — solves this with a single trivial method on the wrapper, not by
> forbidding wrapping middleware on streaming routes entirely. ADR-0013
> mounts `middleware.Capture` (now `Unwrap()`-safe) on `/v1/messages`,
> including its streaming path, and removes the "Option C" post-stream
> logging this ADR's Decision section describes below (`StreamWriter` no
> longer logs the exchange itself). The single-writer invariant — exactly
> one goroutine ever writes SSE bytes to the response for one exchange — is
> the one part of this ADR that remains fully accurate and binding; it was
> never actually a middleware constraint, only conflated with one here.
> The rest of this document is kept for historical context only.

# ADR-0004: SSE single-writer goroutine + Option C post-stream logging

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

Superseded by ADR-0013 (2026-09-04) — see the notice at the top of this
document. Originally: Accepted

## Date

2026-08-22

## Context

Two related failure modes needed a design answer, not a patch:

1. **The Flusher-forwarding bug class.** If any middleware wraps the HTTP
   response writer on a streaming route and that wrapper doesn't also
   implement and forward `http.Flusher`, the stream silently stops being
   a stream — bytes sit in a buffer until the handler returns, and the
   client sees nothing until the whole response is done. This is an easy
   mistake to reintroduce every time a new middleware gets added, unless
   the architecture makes it structurally impossible.
2. **Where to log a streamed exchange.** A per-event exchange-log write
   would mean the logger and the writer goroutine both touch shared state
   on every SSE event — either a lock per event (throughput cost on a
   hot path) or a redesign of what "the exchange" means for logging
   purposes.

## Options Considered

### Option 1: Per-event logging with a shared logger lock

**Pros:**

- Near real-time log visibility as events are sent.

**Cons:**

- Adds lock contention on the hottest path in the service for a benefit nothing currently
  needs.

### Option 2: Middleware-wrapped writer with explicit Flusher forwarding

**Pros:**

- Lets middleware stay in the normal Echo middleware chain.

**Cons:**

- Technically workable, but every future middleware addition would need to remember to
  forward `http.Flusher` correctly — the exact bug class this decision exists to prevent.

### Option 3: Single writer goroutine + Option C post-stream logging (chosen)

**Pros:**

- The Flusher-forwarding bug class is eliminated by construction, not by code review
  vigilance.
- No lock contention on the streaming hot path — the writer goroutine owns the response
  writer alone.

**Cons:**

- Exchange-log entries for streamed turns appear after the stream finishes, not
  incrementally.

## Decision

- **Single writer.** Exactly one goroutine owns the response writer for a
  streaming request. It reads a channel of backend events and writes +
  flushes each one as it arrives. No middleware wraps the response writer
  on streaming routes at all — this isn't a coding convention to remember,
  it's enforced by which middleware are mounted on which routes.
- **Option C: post-stream logging.** The writer goroutine accumulates the
  events it sent as it sends them, and hands the accumulated exchange to
  the logger once the event channel closes — after the response is fully
  sent, not interleaved with sending it. Unary (non-streaming) routes keep
  a normal request/response capture middleware, since there's no writer
  contention to avoid there.

Cleanup contract: the channel is closed and `ctx.Done()` is watched so no
write happens after the handler has returned control.

## Consequences

### Positive

- The Flusher-forwarding bug class is eliminated by construction, not by code review
  vigilance.
- Any future middleware that needs to observe streaming traffic has to do so through the
  writer goroutine's own hooks, not by wrapping the response writer — this is a real
  constraint on future work, documented here so it isn't rediscovered by reintroducing the
  bug.

### Negative

- Exchange-log entries for streamed turns appear after the stream finishes, not
  incrementally.

### Mitigation

- Accepted as-is: the log's job is post-hoc correlation and troubleshooting, not live
  tailing of an in-progress response.

## References

- `core/domain/translator/sse_bridge.go`
- `observability/format.go`
