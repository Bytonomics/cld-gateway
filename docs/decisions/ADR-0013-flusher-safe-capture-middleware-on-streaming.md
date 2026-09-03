---
type: decision
title: "ADR-0013: Flusher-safe Capture middleware covers streaming routes"
status: stable
tags:
  - adr
  - streaming
  - observability
  - error-handling
generated:
  by: claude-sonnet-5
  at: 2026-09-04T00:00:00Z
---

# ADR-0013: Flusher-safe Capture middleware covers streaming routes

| Section | What it covers |
|---------|----------------|
| [Status](#status) | Where this decision stands |
| [Date](#date) | When it was decided |
| [Context](#context) | Why ADR-0004's constraint no longer holds |
| [Options Considered](#options-considered) | The alternatives with pros and cons |
| [Decision](#decision) | What was chosen and why |
| [Consequences](#consequences) | Positive, negative, and mitigations |
| [References](#references) | Sources and related decisions |

## Status

Accepted. Supersedes ADR-0004's "no middleware wraps the response writer on
streaming routes" constraint. ADR-0004's Option C post-stream logging
mechanism (`StreamWriter` calling `log.Append` itself) is removed; its
single-writer-owns-the-response-bytes invariant is unchanged and still
binding.

## Date

2026-09-04

## Context

ADR-0004 was written on the premise that any middleware wrapping the HTTP
response writer on a streaming route would need to remember to forward
`http.Flusher` correctly, and that this was "an easy mistake to reintroduce
every time a new middleware gets added" — so it made this impossible by
construction: no middleware may wrap the writer on streaming routes at all,
full stop.

That premise was true for the pre-Go-1.20 pattern of implementing
`http.Flusher` by hand on every wrapper type. It stopped being the whole
picture once Go 1.20 shipped `http.ResponseController`
(https://pkg.go.dev/net/http#ResponseController) and its `Unwrap() http.ResponseWriter`
convention. Verified directly against this module's own dependencies, not
assumed:

- `echo.Response.Flush()` (`response.go:88-93` in the installed
  `github.com/labstack/echo/v4 v4.13.4` module) does not type-assert
  `http.Flusher` on its writer directly. It calls
  `http.NewResponseController(r.Writer).Flush()`.
- `http.ResponseController`, when the immediate writer doesn't implement
  `http.Flusher` itself, looks for an `Unwrap() http.ResponseWriter` method
  on it and recurses through the chain until it finds a real one.
- This module's `go.mod` targets `go 1.25.0` — the `ResponseController`
  mechanism has been available since 1.20.

So a response-writer wrapper needs exactly one trivial method —
`Unwrap() http.ResponseWriter { return w.ResponseWriter }` — to be
completely safe to use on a streaming route. This is not a workaround
specific to one wrapper; it is the standard library's own answer to the
exact problem ADR-0004 was defending against, and it applies uniformly to
any future middleware that adopts the same one-method convention.

Separately, re-reading `handlers/messages.go`'s actual call to
`StreamWriter.Run` (unchanged by this decision) showed it was always called
synchronously, not handed off to a detached goroutine outside the request's
control flow. The HTTP handler function blocks for the full duration of the
stream and only returns once `Run` returns. ADR-0004's framing implied the
response writer was "handed off" in a way that put it beyond a wrapping
middleware's reach entirely; that was an overstatement. The real, narrower
blocker was only ever the Flusher-forwarding gap, not the synchronous call
structure.

This also closes a real, separate gap found in production use: because
`/v1/messages` carried no `middleware.Capture`, every error that occurred
before or during stream setup (a bind failure, a service error) had to be
logged via hand-written, duplicated logic in `handlers/messages.go`
(`logError`/`logUnary`), instead of going through the same single
`middleware.ClassifyForResponse` path every other route already used. That
duplication is exactly the kind of drift ADR-0005 was written to prevent.

## Options Considered

### Option 1: Keep ADR-0004 as-is, accept the duplicated logging path

**Pros:**

- No change to a working system.

**Cons:**

- `/v1/messages` remains the one route with its own hand-rolled exchange
  logging, duplicating logic that exists once, correctly, in
  `middleware.Capture`/`middleware.ClassifyForResponse` for every other
  route.
- The premise behind the duplication (middleware can't safely wrap
  streaming) is no longer accurate given Go 1.20's `ResponseController`.

### Option 2: Add `Unwrap()` to the wrapper, mount `Capture` on `/v1/messages` (chosen)

**Pros:**

- One `middleware.ClassifyForResponse` call site for every route's error
  path, streaming included — the property ADR-0005 already established for
  the other five routes now genuinely holds for all six.
- `middleware.Capture` becomes the single place that logs the exchange for
  `/v1/messages`, unary and streaming both — `StreamWriter`'s own separate
  Option C logging is removed, eliminating a class of double-logging risk
  if both had been kept.
- The fix (`Unwrap()`) is a standard-library-blessed, one-method pattern,
  not a bespoke hack — any future middleware gets the same safety by
  implementing the same method, rather than needing bespoke Flusher
  forwarding remembered per middleware (the exact failure mode ADR-0004
  worried about, now solved structurally instead of by discipline).

**Cons:**

- `StreamWriter.Run`'s signature changes (drops the `log`/`base` parameters
  it used to take), requiring every call site to be updated.
- Per-request metadata `Capture` can't know about generically (e.g.
  `dto.MessagesResponse.ContextManagementMetadata`) has to be threaded
  through `c.Set`/`c.Get` on the Echo context instead of being passed as an
  explicit parameter — a slightly less type-safe path than before.

### Option 3: Redesign streaming to avoid Echo entirely for `/v1/messages`

**Pros:**

- Sidesteps the question of Echo-level Flusher forwarding altogether.

**Cons:**

- A far larger rewrite for no benefit once Option 2's one-method fix is
  available; rejected without further exploration.

## Decision

- `middleware.Capture`'s `captureWriter` implements
  `Unwrap() http.ResponseWriter`, making it safe to mount on any route,
  streaming included.
- `middleware.Capture` is now mounted on `POST /v1/messages`
  (`app/routes_messages.go`), alongside the five routes it already covered.
- `middleware.Capture` gains metadata support: a handler sets
  `c.Set("exchange_metadata", v)` before returning; `Capture` reads it back
  after `next(c)` to populate `Entry.Metadata`.
- `handlers/messages.go`'s `logError`/`logUnary`/`requestRecord` methods
  and its `observability.ExchangeLog` dependency are removed entirely —
  `Capture` is now the sole logger for this route.
- `core/impl/services/stream_writer.go`'s `StreamWriter.Run` no longer
  takes `StreamLogEntry`/`observability.ExchangeLog` parameters and no
  longer calls `log.Append` itself; `StreamLogEntry` is removed. The
  single-writer invariant itself — exactly one goroutine ever writes SSE
  bytes to `c.Response()` for the lifetime of one streaming exchange — is
  unchanged; that invariant was always about not racing two goroutines on
  one writer, never about middleware, and remains fully binding.
- `finalizeErrorEvent`'s role (in-band SSE `"error"` events for mid-stream
  aborts, since HTTP status/headers are already committed by that point) is
  unchanged. This decision does not and cannot eliminate that second
  `errors.Classify` call site — HTTP-level middleware cannot rewrite a
  status code that has already been sent to the client, which is why SSE
  error handling is universally pushed to in-band events at this point in
  a stream, independent of any framework's middleware capabilities.

## Consequences

### Positive

- Exactly one exchange-logging code path for all six routes, not five plus
  one hand-rolled duplicate.
- Exactly one HTTP-level error-classification call site
  (`middleware.ClassifyForResponse`) covers every route's pre-stream error
  path, closing the gap where `/v1/messages` previously diverged.
- The Flusher-forwarding safety property ADR-0004 wanted is preserved, but
  now structurally guaranteed by the standard library's own mechanism
  rather than by forbidding a whole category of middleware.

### Negative

- `StreamWriter.Run`'s signature is a breaking change for any future caller
  outside this repo (none exist today, confirmed by repo-wide search before
  this change).
- Per-request log metadata now flows through untyped `c.Set`/`c.Get`
  instead of an explicit struct parameter.

### Mitigation

- The metadata key (`"exchange_metadata"`) is a single, documented,
  package-private constant (`exchangeMetadataContextKey` in
  `middleware/capture.go`), not a string repeated at each call site.

## References

- `middleware/capture.go`
- `handlers/messages.go`
- `core/impl/services/stream_writer.go`
- `ADR-0004` (superseded by this decision)
- `ADR-0005` (the single-classification-point principle this decision
  extends to cover the sixth route)
- Go stdlib: `net/http.ResponseController`
  (https://pkg.go.dev/net/http#ResponseController), available since Go 1.20
