---
type: decision
title: "ADR-0007: Conversation state stays core, not a pluggable port"
status: stable
tags:
  - adr
  - architecture
generated:
  by: claude-sonnet-5
  at: 2026-09-03T00:00:00Z
---

# ADR-0007: Conversation state stays core, not a pluggable port

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

Every other major subsystem in this port — backend, auth — sits behind an interface in
`core/domain/port` precisely because more than one implementation is a realistic near-term
need. Conversation state (`ConversationRepo`, `ToolCallRepo`) is architecturally similar in
shape (an interface with a filesystem/SQLite-backed implementation) but different in what it
actually protects: the on-disk format — branches, sparse checkpoints, a JSONL ledger,
retention policy, corruption handling — is a load-bearing contract that the daemon's own
crash recovery and restart behavior depend on, not an interchangeable storage backend.

## Options Considered

### Option 1: Fully pluggable state backend (e.g. swap to Postgres via config)

**Pros:**

- Matches the pluggable-port pattern already used for `Backend` and auth, so it would be
  architecturally consistent with the rest of the port.

**Cons:**

- No concrete need for it exists yet.
- The correctness properties (retention, corruption policy, crash recovery) are exactly the
  kind of thing that gets subtly wrong when reimplemented against a second backend under
  time pressure.

### Option 2: Core domain logic with one canonical implementation

**Pros:**

- The 20-method `ConversationRepo` surface stays a 1:1 port of the Rust
  `ConversationStateStore`, so the on-disk format keeps working exactly as before, including
  for state written by the Rust binary during the coexistence period.
- The interface boundary still exists for testability (fakes in unit tests) and code
  organization.

**Cons:**

- Anyone tempted to add a second `ConversationRepo` implementation later has to treat that
  as a much bigger decision than adding a second `Backend` implementation.

## Decision

`ConversationRepo` and `ToolCallRepo` live in `core/domain/port/state` and have their
interfaces defined there like any other port, but they are treated as core domain logic with
one canonical implementation (filesystem JSON/JSONL for conversation state, GORM+glebarez
SQLite for tool calls) rather than as a pluggability seam meant to support swapping storage
engines. The 20-method `ConversationRepo` surface is a 1:1 port of the Rust
`ConversationStateStore`, kept intact rather than trimmed or reshaped, because the on-disk
format it manages needs to keep working exactly as before, including for state written by
the Rust binary during the coexistence period
([ADR-0012](ADR-0012-rust-retained-until-parity-cutover.md)).

## Consequences

### Positive

- The interface boundary exists for testability (fakes in unit tests) and code
  organization, not because a second production implementation is expected. That's a
  different reason to have an interface than the `Backend` port has, and contributors
  should read it that way.

### Negative

- Anyone tempted to add a second `ConversationRepo` implementation for a different storage
  engine should treat that as a much bigger decision than adding a second `Backend`
  implementation.

### Mitigation

- It means committing to keeping two on-disk formats compatible with the same
  crash-recovery and retention guarantees — revisit only if a real need for a second
  storage engine appears.

## References

- [ADR-0012: Rust retained until parity cutover, then one-commit delete](ADR-0012-rust-retained-until-parity-cutover.md)
- `core/domain/port/state/`
