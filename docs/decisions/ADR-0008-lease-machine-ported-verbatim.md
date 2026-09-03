---
type: decision
title: "ADR-0008: Lease machine ported verbatim"
status: stable
tags:
  - adr
  - correctness
generated:
  by: claude-sonnet-5
  at: 2026-09-03T00:00:00Z
---

# ADR-0008: Lease machine ported verbatim

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

Accepted — not open for redesign as part of this port

## Date

2026-08-22

## Context

The main-turn lease state machine exists to solve one specific
correctness problem: a client that disconnects mid-turn must never cause
that turn to be committed to conversation state as if the client had seen
it complete. Without this, an aborted request could silently leave a
"completed" turn in history that the user never actually witnessed
finishing — a state the user has no way to notice or correct after the
fact.

This is a correctness property with real consequences (corrupted-looking
conversation history), not a performance optimization. A language port is
exactly the kind of change where subtly altering state-machine semantics
while translating syntax is easy to do by accident.

## Options Considered

### Option 1: Simplify the state machine during the port

**Pros:**

- Fewer states would be easier to read and reason about at a glance.

**Cons:**

- The states encode specific abort/failure scenarios the Rust implementation was built to
  handle, and collapsing them risks losing distinctions that actually matter for
  correctness.

### Option 2: Port the lease state machine verbatim

**Pros:**

- Preserves a correctness property (no falsely-committed turns) with real consequences for
  conversation history, exactly as validated in the Rust implementation.
- Removes the risk of subtly altering state-machine semantics while translating syntax, a
  known hazard of language ports.

**Cons:**

- Ties the Go implementation to the Rust design even where a cleaner Go-native shape might
  exist, until a deliberate, reviewed redesign happens.

## Decision

The lease state machine is ported verbatim: same states, same transition table, same
`AllowsCommit()` gating logic.

```
in_flight → completed_committed
          → client_aborted_before_first_event
          → client_aborted_after_visible_output
          → backend_failed_before_commit
          → commit_suppressed_after_abort
```

Only `in_flight` allows a commit. The per-session mutex registry that serializes lease
operations per session is ported the same way. This combination is called out explicitly in
[ARCHITECTURE_v2](../ARCHITECTURE_v2.md) as a CORE FEATURE not subject to the general
license to reshape things during porting.

[GAPS.md](../GAPS.md) G2 (leases are memory-only, lost on process restart) is a separate,
explicitly *open* decision — see
[ADR-0010](ADR-0010-approved-gap-fixes-rust-review.md) — and does not change the state
machine itself; it's about whether lease state survives a crash, not what the states mean.

## Consequences

### Positive

- The port's job here was translation, not improvement — a real bug found in the lease
  machine's logic itself is a bug report against the *design*, to be fixed deliberately and
  on both implementations during the coexistence period, not folded silently into "the Go
  version."

### Negative

- Anyone touching `core/domain/transport/lease.go` or its implementation has to treat a
  proposed behavior change here as a correctness review, not a refactor.

### Mitigation

- Require a second reviewer who understands the original aborted-client problem before
  changing transition logic.

## References

- [ADR-0010: Approved gap fixes from the Rust implementation review](ADR-0010-approved-gap-fixes-rust-review.md)
- `core/domain/transport/lease.go`
