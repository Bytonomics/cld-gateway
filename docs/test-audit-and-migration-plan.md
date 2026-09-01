# TODO: Full test audit + migration plan (parked)

Status: parked — execute before writing Go code, after the design is final.

## Context

The Rust crates carry a large inline test suite (~40% of
`gateway-http-anthropic/src/lib.rs` lines are tests). The owner suspects a
meaningful fraction are "fake tests" — tests that assert implementation
details or restate the code rather than protect behavior. Behavioral parity
of the Go port depends on knowing which tests actually encode contracts.

## Work to do

1. Enumerate every test in every crate under `crates/`
   (unit tests, wiremock integration tests, ws-mock tests).
2. For each test, label it:
   - CONTRACT: asserts externally observable behavior worth preserving.
   - FAKE: asserts internals, mirrors the implementation, or tests nothing
     meaningful — candidate for dropping.
   - SLOP-ADJACENT: depends on the prompt-text heuristics in `AI_SLOP.md`
     — must not be ported; behavior intentionally changes.
3. Produce a migration matrix: each CONTRACT test → the Go test that will
   replace it (package, seam, fixtures needed).
4. Identify scenario gaps: behaviors with no CONTRACT test today; decide
   whether to add coverage in Go even though Rust never had it.
5. Estimate the phased implementation order from the matrix
   (core → state → backend → translate → app → httpapi → cmd).

## Deliverable

A written migration plan (this folder or `golang_port/docs/`) listing:
kept tests, dropped tests with one-line reasons, new Go tests per package,
and the phased build order.

## Trigger to unpark

Owner confirms the golang_port design interview is complete.
