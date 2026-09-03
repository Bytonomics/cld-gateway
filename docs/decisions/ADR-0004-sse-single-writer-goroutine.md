---
type: decision
title: "ADR-0004: SSE single-writer goroutine + Option C post-stream logging"
status: stable
tags:
  - adr
  - streaming
generated:
  by: claude-sonnet-5
  at: 2026-09-03T00:00:00Z
---

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

Accepted

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
