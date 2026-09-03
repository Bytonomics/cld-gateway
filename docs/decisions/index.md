---
type: reference
title: "Architecture Decision Records"
status: stable
tags:
  - adr
  - index
stale_after: 2026-12-02
generated:
  by: claude-sonnet-5
  at: 2026-09-03T00:00:00Z
---

# Architecture Decision Records

Numbered decision records for the Rust→Go port of cld-gateway. Each ADR
captures one decision, the alternatives considered, and why the chosen
option won — not a running log of implementation notes.

| ADR | Title |
|---|---|
| [ADR-0001](ADR-0001-claude-code-inbound-only.md) | 1-to-n scope: Claude Code inbound only |
| [ADR-0002](ADR-0002-ddd-layout-single-go-module.md) | smritea-style DDD layout, single Go module |
| [ADR-0003](ADR-0003-echo-pedantigo.md) | Echo + pedantigoecho + pedantigo v2 |
| [ADR-0004](ADR-0004-sse-single-writer-goroutine.md) | SSE single-writer goroutine + Option C post-stream logging (**superseded by ADR-0013**) |
| [ADR-0005](ADR-0005-apperror-anthropic-error-shape.md) | AppError → Anthropic error shape |
| [ADR-0006](ADR-0006-backend-port-composition-translators.md) | Backend port + extend-via-composition translators |
| [ADR-0007](ADR-0007-conversation-state-core-not-pluggable.md) | Conversation state stays core, not a pluggable port |
| [ADR-0008](ADR-0008-lease-machine-ported-verbatim.md) | Lease machine ported verbatim |
| [ADR-0009](ADR-0009-library-selections.md) | Library selections |
| [ADR-0010](ADR-0010-approved-gap-fixes-rust-review.md) | Approved gap fixes from the Rust implementation review |
| [ADR-0011](ADR-0011-formatted-text-exchange-log-format.md) | Formatted-text exchange log format |
| [ADR-0012](ADR-0012-rust-retained-until-parity-cutover.md) | Rust retained until parity cutover, then one-commit delete |
| [ADR-0013](ADR-0013-flusher-safe-capture-middleware-on-streaming.md) | Flusher-safe Capture middleware covers streaming routes (supersedes ADR-0004) |

Source material for these records: [ARCHITECTURE_v2.md](../ARCHITECTURE_v2.md),
[GAPS.md](../GAPS.md), [AI_SLOP.md](../AI_SLOP.md), and the design-interview decision
ledger.
