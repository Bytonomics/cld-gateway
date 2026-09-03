---
type: decision
title: "ADR-0012: Rust retained until parity cutover, then one-commit delete"
status: stable
tags:
  - adr
  - migration
generated:
  by: claude-sonnet-5
  at: 2026-09-03T00:00:00Z
---

# ADR-0012: Rust retained until parity cutover, then one-commit delete

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

A rewrite of a running production service raises an obvious question: when does the old
implementation actually go away? Deleting Rust source incrementally as Go packages reach
feature parity is tempting — it keeps the diff smaller at each step — but it also means
there's no single point where "the port is done and Rust is gone" is true; the repository
spends an extended period in a state where neither implementation is complete, and a
rollback from a bad Go release has to reconstruct whatever Rust pieces were already deleted.

## Options Considered

### Option 1: Incremental crate deletion as Go packages land

**Pros:**

- Keeps each individual diff smaller.

**Cons:**

- No clean "done" state until every crate is gone, and a partial deletion midway through is
  a worse rollback position than either "all Rust" or "no Rust."

### Option 2: Parallel maintenance indefinitely (never delete Rust)

**Pros:**

- Never forces the cutover decision; both implementations stay available forever.

**Cons:**

- Defers the actual decision this port exists to make, and doubles ongoing maintenance
  cost for both implementations indefinitely.

### Option 3: Retain Rust untouched until parity, then one-commit delete (chosen)

**Pros:**

- A clean rollback story: reverting before the cutover is a normal `git revert` of
  Go-only changes; reverting after it is a revert of one commit that restores the entire
  Rust tree at once.
- A single, explicit point where "the port is done and Rust is gone" becomes true.

**Cons:**

- The repository carries two working implementations of the same service simultaneously
  for the length of the port — a real cost in repository size and "which one is real"
  clarity for anyone new to the codebase.

## Decision

Rust stays in the repository, untouched and fully functional, for the
entire duration of the Go port's development. It remains the release
daily-driver — the thing actual users run — until the Go port has both
reached behavioral parity *and* has itself run as the daily driver for a
period the owner is satisfied with. Only then is the Rust source deleted,
in one commit.

Rollback story: reverting to Rust before that cutover commit is a normal
`git revert` of Go-only changes (Rust was never touched). Reverting after
the cutover commit is a revert of that single commit, which restores the
entire Rust tree at once rather than requiring reconstruction from
history.

## Consequences

### Positive

- No Go package is considered "done, Rust equivalent can be deleted" on its own — parity
  is judged at the whole-service level, not package-by-package, because a
  partially-deleted Rust tree isn't a working fallback.
- The release pipeline itself needs no branching logic for "which implementation builds
  this release" during the coexistence period — see [Releasing](../runbooks/releasing.md)
  and [RELEASE_INTEGRATION.md](../RELEASE_INTEGRATION.md) for what actually changes in the
  release pipeline once the Go binary becomes the thing being shipped.

### Negative

- The repository carries two working implementations of the same service simultaneously
  for the length of the port — a real cost in repository size and "which one is real"
  clarity for anyone new to the codebase.

### Mitigation

- Accepted deliberately in exchange for a clean rollback story.

## References

- [Releasing](../runbooks/releasing.md)
- [RELEASE_INTEGRATION.md](../RELEASE_INTEGRATION.md)
- `old_rust/`
