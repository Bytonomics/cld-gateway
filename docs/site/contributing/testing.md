# Testing

## The CONTRACT / FAKE / SLOP-ADJACENT labeling

The Rust test suite is large — roughly 40% of `gateway-http-anthropic`'s
source lines are tests — and not every test earns a place in the Go port
by default. Before porting a Rust test, label it:

- **CONTRACT** — asserts externally observable behavior worth preserving
  (a request shape produces a specific response shape, a state transition
  happens under specific conditions, an error surfaces with the right
  code). Port these.
- **FAKE** — asserts internal implementation details, restates what the
  code does rather than what it guarantees, or doesn't actually protect
  any behavior a caller depends on. Candidate for dropping rather than
  porting — a test that would need to change every time an implementation
  detail changes isn't protecting a contract, it's adding friction.
- **SLOP-ADJACENT** — depends on the prompt-text heuristics documented in
  `AI_SLOP.md`. Do not port these as-is; the underlying behavior they test
  is either being replaced with a deterministic check (see
  [Design decisions](../contributing/design-decisions.md)) or, for the
  cases kept-but-marked-`BUG`, the test needs to assert the *current*
  (buggy, marked) behavior explicitly rather than silently validate a
  heuristic as if it were correct.

Every test gets this label before it's ported, not after — the label
decides whether it's ported at all.

## Migration matrix

For each CONTRACT-labeled test, the migration plan records: which Go
package now owns that behavior, which test seam exercises it, and what
fixtures (golden files, wiremock stubs) it needs. This turns "port the
Rust tests" from a mechanical file-by-file translation into a deliberate
mapping from behavior to new test — a test that ported cleanly onto a
different package boundary than its Rust counterpart is still doing its
job; a test that ported onto the same package but stopped asserting
anything meaningful is not.

Full audit and phased build order (core → state → backend → translate →
app → HTTP API → CLI) live in `golang_port/docs/test-audit-and-migration-plan.md`
— that document is the actual working matrix; this page describes the
method, not the line-by-line result.

## Seam choice: the HTTP boundary

The primary test seam for behavior-level tests is the HTTP boundary — send
a real request into the router, assert on the real HTTP response
(including SSE event sequence for streaming routes), with the backend
faked underneath via the `Backend` port interface. This mirrors how the
Rust suite's wiremock-based integration tests worked, and it means a test
survives internal refactoring inside `core/impl` as long as the observable
request/response contract doesn't change — exactly the CONTRACT-vs-FAKE
distinction applied structurally.

## Flush-invariant stream tests

Because the single-writer SSE model ([ADR-0004](adr/ADR-0004.md)) exists
specifically to guarantee bytes reach the client per event rather than
being buffered until the handler returns, streaming tests assert on that
invariant directly — that each SSE event is observable on the response as
it's produced, not just that the final accumulated stream is correct.
A test that only checks the final buffered output would not catch a
regression back into the Flusher-forwarding bug class that ADR exists to
prevent.

## Golden files

Where a translation or response-shaping function's output is verbose
enough that inline assertions would be unreadable, tests compare against
a golden file rather than a hand-written expected value inline. Golden
files are checked in next to their test and updated deliberately (not via
an update-all-goldens script run without review) when a behavior change
is intentional.
