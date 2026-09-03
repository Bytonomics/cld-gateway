---
type: plan
title: "Plan: Full Test Audit and Migration Matrix"
status: draft
tags:
  - testing
  - parked
  - go-port
generated:
  by: claude-sonnet-5
  at: 2026-09-03T00:00:00Z
---

# TODO: Full test audit + migration plan (parked)

| Section | What it covers |
|---------|----------------|
| [Context](#context) | Why Rust's inline test suite needs a CONTRACT/FAKE/SLOP-ADJACENT audit |
| [Work to do](#work-to-do) | Enumerate, label, and produce a migration matrix |
| [Deliverable](#deliverable) | The written migration plan this work produces |
| [Trigger to unpark](#trigger-to-unpark) | The condition that must be met before this work starts |

Status: parked. Originally scoped to run before any Go code was written; Go is now the root
implementation (`cmd/`, `app/`, `core/`, `handlers/`, etc.) and already carries its own test
suite (see `docs/TEST_PLAN.md` for the target seams and build sequence). This document's specific
deliverable — enumerating and labeling every existing Rust test as CONTRACT/FAKE/SLOP-ADJACENT —
has not been produced, so the audit itself remains open work; the [trigger to unpark](#trigger-to-unpark)
below still governs when to pick it up.

## Context

The frozen Rust implementation (`old_rust/crates/`) carries a large inline test suite (~40% of
`gateway-http-anthropic/src/lib.rs` lines are tests). The owner suspects a
meaningful fraction are "fake tests" — tests that assert implementation
details or restate the code rather than protect behavior. Behavioral parity
of the Go port depends on knowing which tests actually encode contracts.

## Work to do

1. Enumerate every test in every crate under `old_rust/crates/`
   (unit tests, wiremock integration tests, ws-mock tests).
2. For each test, label it:
   - CONTRACT: asserts externally observable behavior worth preserving.
   - FAKE: asserts internals, mirrors the implementation, or tests nothing
     meaningful — candidate for dropping.
   - SLOP-ADJACENT: depends on the prompt-text heuristics in `docs/AI_SLOP.md`
     — must not be ported; behavior intentionally changes.
3. Produce a migration matrix: each CONTRACT test → the Go test that will
   replace it (package, seam, fixtures needed).
4. Identify scenario gaps: behaviors with no CONTRACT test today; decide
   whether to add coverage in Go even though Rust never had it.
5. Estimate the phased implementation order from the matrix
   (core → state → backend → translate → app → httpapi → cmd).

## Deliverable

A written migration plan (this folder, `docs/`) listing:
kept tests, dropped tests with one-line reasons, new Go tests per package,
and the phased build order.

## Trigger to unpark

Owner confirms the golang_port design interview is complete.
