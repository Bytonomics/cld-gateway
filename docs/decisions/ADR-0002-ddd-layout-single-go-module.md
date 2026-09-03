---
type: decision
title: "ADR-0002: smritea-style DDD layout, single Go module"
status: stable
tags:
  - adr
  - architecture
generated:
  by: claude-sonnet-5
  at: 2026-09-03T00:00:00Z
---

# ADR-0002: smritea-style DDD layout, single Go module

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

The Rust source is a Cargo workspace of seven crates (`gateway-core`, `gateway-auth-codex`,
`gateway-backend-codex`, `gateway-http-anthropic`, `gateway-state`, `gateway-net`,
`gateway-observability`) with explicit allowed/not-allowed dependency notes at the top of
each crate's `lib.rs`. Go has no first-class workspace concept comparable to a Cargo
workspace for a project this size, and Go's own visibility idiom (an *internal/* directory)
plus a single `go.mod` covers the same "some packages are implementation detail, not public
API" need that crate boundaries covered in Rust.

## Options Considered

### Option 1: Go workspace (`go.work`) mirroring the Cargo crate split

**Pros:**

- Directly mirrors the Rust crate boundaries, one-to-one.

**Cons:**

- Adds multi-module overhead (a separate `go.mod` per crate-equivalent) for a project that
  doesn't need independently versioned or independently released components.

### Option 2: Single Go module, DDD-derived package layout

**Pros:**

- `smritea-cloud`'s single-module layout was already verified to work at comparable scope.
- The dependency rule (domain defines, impl satisfies, app wires) is enforced by package
  structure and compile-time interface assertions rather than by a workspace dependency
  graph.
- No workspace-level version skew to manage — one `go.mod`, one build.

**Cons:**

- The Rust crate list and the Go package list are not 1:1, so a reader moving between the
  two codebases has to map by responsibility rather than by directory name.

## Decision

Single Go module, *github.com/Bytonomics/cld-gateway*, one `go.mod`, no workspace. Package
boundaries are re-derived from the *interfaces* the Rust crates expose — ports, services,
adapters — using a layout already proven in `smritea-cloud`: `core/domain` (interfaces +
DTOs, zero concrete dependencies), `core/impl` (concrete implementations), `app` (manual
constructor DI), `handlers`/`middleware`/`observability` as thin outer layers.

## Consequences

### Positive

- The dependency rule (domain defines, impl satisfies, app wires) is enforced by package
  structure and compile-time interface assertions (`var _ services.MessageService =
  (*MessageService)(nil)`) rather than by Cargo's workspace dependency graph.
- No workspace-level version skew to manage — one `go.mod`, one build.

### Negative

- The Rust crate list and the Go package list are *not* 1:1; a reader going from one
  codebase to the other should map by responsibility (which interface, which adapter)
  rather than by looking for a same-named directory.

### Mitigation

- None needed — the mapping-by-responsibility cost is accepted as the tradeoff for a
  simpler single-module build.

## References

- `core/domain/`
- `core/impl/`
