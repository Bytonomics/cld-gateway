---
type: decision
title: "ADR-0010: Approved gap fixes from the Rust implementation review"
status: stable
tags:
  - adr
  - reliability
generated:
  by: claude-sonnet-5
  at: 2026-09-03T00:00:00Z
---

# ADR-0010: Approved gap fixes from the Rust implementation review

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

Partially Accepted — several items remain open (see [GAPS.md](../GAPS.md); a later audit in
[deferred-work-audit-triage](../runbooks/deferred-work-audit-triage.md) re-checked some of
these items, including G1, G3, and a related gap G10, against the codebase)

## Date

2026-08-22

## Context

A critical review of `old_rust/crates/` (`GAPS.md`) turned up thirteen findings ranging from
critical (state lost on restart, no graceful shutdown) to low (no metrics). Each cites
specific Rust source locations. The port needed an explicit per-finding decision rather than
either porting Rust's behavior unexamined or silently "fixing everything" without owner
sign-off — a plan that leaves gaps dangling is exactly what this decision record exists to
avoid.

## Options Considered

### Option 1: Port every finding as a fix

**Pros:**

- Nothing is left open; every gap gets closed in one pass.

**Cons:**

- Several open items need design work (crash-recovery semantics, second-instance
  detection) that a mechanical port pass shouldn't improvise.

### Option 2: Port nothing, exact parity with Rust's current gaps

**Pros:**

- Simplest possible scope for the port — behavior stays identical to Rust's, gaps included.

**Cons:**

- Some fixes (G3, G4, G5, G7) are cheap, clearly correct, and Go's runtime makes several of
  them easier than they were in Rust — declining them "for parity" would be parity with an
  acknowledged shortcoming for no benefit.

### Option 3: Explicit per-finding decision — approve some, leave some open (chosen)

**Pros:**

- Cheap, clearly-correct fixes (G3, G4, G5, G7, G8, G9, G12) land immediately.
- Items needing real design work stay explicitly open rather than being silently ported or
  silently skipped.

**Cons:**

- Requires tracking which items are approved vs. open and revisiting the open ones later
  (see [the deferred-work audit triage runbook](../runbooks/deferred-work-audit-triage.md)).

## Decision

Approved for the Go port, with the fix folded into the relevant file
map entries (marked ✱ in `FILEMAP.md`):

- **G4 — default backend timeout is `None`.** Go port defaults to a 120s
  unary timeout; streaming requests use an idle-event timeout instead of a
  flat deadline, since streams are event-driven and a fixed timeout would
  kill a slow-but-healthy long stream. Both configurable per backend.
- **G5 — swallowed auth-cleanup errors.** Logout-with-revoke failures are
  logged, never silently dropped. Logout itself stays best-effort by
  design (a failed revoke shouldn't block local logout), but "best-effort"
  and "silent" are different things.
- **G7 — swallowed logging errors.** Exchange-log append failures are
  logged via `slog`, with a circuit breaker that disables logging after N
  consecutive failures — protects against a broken log sink generating
  per-request error spam that could itself become a problem.
- **G8 — blocking file IO on the request path.** Go's goroutine model
  makes this a non-issue compared to Rust's async executor concern; the
  fix that does carry over is keeping the per-session lock that
  serializes disk writes, since writes stay small (JSON metadata + JSONL
  append) and don't need exotic async filesystem handling.
- **G9 — unbounded exchange-log growth.** Size-based rotation (rotate
  around 50MB) *and* retention that deletes old rotated files, applied to
  both the formatted-text exchange log
  ([ADR-0011](ADR-0011-formatted-text-exchange-log-format.md)) and the
  JSONL sinks (transport-decisions log).
- **G3 — no graceful shutdown.** Not from the numbered gap list's Medium
  tier but approved outright: `signal.NotifyContext` + `http.Server.Shutdown`
  with a drain timeout, releasing leases on drain. Go makes this
  straightforward where Rust's version had nothing — there was no reason
  not to do it.
- **G12 — no metrics.** Scoped, not implemented at launch: an optional
  OpenTelemetry endpoint, opt-in, shipped later. The observability package
  is designed so this can be added without restructuring — a design
  constraint honored now even though the feature itself is deferred.

Left **open** — not approved, not rejected, pending further design work
before the file map for them freezes:

- **G1 — chain-checkpoint store lost on restart.** Correctness is
  preserved today (a stale association just triggers a safe fallback to
  full-SSE transport once), so this is a pure efficiency question, not a
  correctness one. Persisting associations into branch metadata is the
  likely direction, not yet committed.
- **G2 — main-turn leases lost on restart.** The lease machine itself is
  fixed ([ADR-0008](ADR-0008-lease-machine-ported-verbatim.md)); this is
  specifically about whether lease state survives a crash. A
  persisted-lease-plus-TTL-sweep design is the likely direction, not yet
  committed.
- **G6 — advisory file locking only.** Real risk is two gateway processes
  interleaving writes; mitigated in practice by the daemon being a
  single-instance service. A second-instance lockfile check is the likely
  direction, not yet committed.
- **G11 — health endpoint is a stub.** Direction (lightweight `/health`
  reporting uptime and config-load state, with a heavier optional
  `/health/deep` later) is sketched but not committed as of this record.

## Consequences

### Positive

- `FILEMAP.md` entries marked ✱ trace directly to an *approved* item in this list — an
  implementer should not add gap-fix behavior beyond what's marked, and should not skip
  what is marked.

### Negative

- The open items are real gaps in both implementations today, not regressions introduced
  by the port.

### Mitigation

- Open items should not block the port's own progress, but they also shouldn't be quietly
  closed by an implementer's own judgment call — each needs an owner decision first.

## References

- [GAPS.md](../GAPS.md)
- [FILEMAP.md](../FILEMAP.md)
- [ADR-0008: Lease machine ported verbatim](ADR-0008-lease-machine-ported-verbatim.md)
- [ADR-0011: Formatted-text exchange log format](ADR-0011-formatted-text-exchange-log-format.md)
