---
type: decision
title: "ADR-0011: Formatted-text exchange log format"
status: stable
tags:
  - adr
  - observability
generated:
  by: claude-sonnet-5
  at: 2026-09-03T00:00:00Z
---

# ADR-0011: Formatted-text exchange log format

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

The Rust implementation's exchange log is JSONL — one JSON object per line. That's a
reasonable format for a downstream consumer parsing the log programmatically, but the
dominant observed use case for this specific log is a human directly reading recent entries,
most often while troubleshooting a specific request by its ID (see
[Troubleshooting](../runbooks/troubleshooting.md)). JSONL is awkward for that: no line-based
structure a human can `grep` a block out of, escaped strings that hide multi-line content, no
visual entry boundary.

## Options Considered

### Option 1: Keep JSONL, add a separate human-readable log

**Pros:**

- Keeps the format Rust used for the primary log, matching downstream tooling expectations.

**Cons:**

- Two log files recording the same information is a maintenance and disk-usage cost for a
  benefit that doesn't actually serve either use case better than picking one format for
  the primary log.

### Option 2: Switch the primary exchange log to formatted text

**Pros:**

- Directly greppable and scannable without a JSON tool — matches the actual primary use
  case (a human troubleshooting a specific request).

**Cons:**

- Anything that wants to consume the exchange log programmatically has to parse the
  `key: value` + separator shape instead of parsing JSON lines.

## Decision

The primary exchange log for the Go port uses a formatted-text shape
instead: one entry per exchange, `key: value` lines, terminated by a
dashed separator line.

```
request_id: 7f3a2c10-...
method: POST
path: /v1/messages
status: 200
duration_ms: 842
------------------------------------
```

This is a user-specified format decision, not a default library output —
the writer (`observability/format.go`) is hand-written to this exact
shape. The separate `transport-decisions.jsonl` sink keeps JSONL, because
that log *is* consumed by tooling (see
[Troubleshooting](../runbooks/troubleshooting.md)) rather than
primarily read by a human.

## Consequences

### Positive

- The exchange log is directly greppable and scannable without a JSON tool — matches the
  actual primary use case.

### Negative

- Anything that wants to consume the exchange log programmatically has to parse the
  `key: value` + separator shape instead of parsing JSON lines.

### Mitigation

- If a real programmatic consumer shows up later, that's a reason to reconsider, but none
  exists today.
- Log rotation and retention (G9, see
  [ADR-0010](ADR-0010-approved-gap-fixes-rust-review.md)) apply to this format the same as
  they would to JSONL — rotation logic doesn't care about the line shape, just file size.

## References

- [ADR-0010: Approved gap fixes from the Rust implementation review](ADR-0010-approved-gap-fixes-rust-review.md)
- [Troubleshooting](../runbooks/troubleshooting.md)
- `observability/format.go`
