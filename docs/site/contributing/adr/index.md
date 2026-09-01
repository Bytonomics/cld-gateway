# Architecture Decision Records

Numbered decision records for the Rust→Go port of cld-gateway. Each ADR
captures one decision, the alternatives considered, and why the chosen
option won — not a running log of implementation notes.

| ADR | Title |
|---|---|
| [ADR-0001](ADR-0001.md) | 1-to-n scope: Claude Code inbound only |
| [ADR-0002](ADR-0002.md) | smritea-style DDD layout, single Go module |
| [ADR-0003](ADR-0003.md) | Echo + pedantigoecho + pedantigo v2 |
| [ADR-0004](ADR-0004.md) | SSE single-writer goroutine + Option C post-stream logging |
| [ADR-0005](ADR-0005.md) | AppError → Anthropic error shape |
| [ADR-0006](ADR-0006.md) | Backend port + extend-via-composition translators |
| [ADR-0007](ADR-0007.md) | Conversation state stays core, not a pluggable port |
| [ADR-0008](ADR-0008.md) | Lease machine ported verbatim |
| [ADR-0009](ADR-0009.md) | Library selections |
| [ADR-0010](ADR-0010.md) | Approved gap fixes from the Rust implementation review |
| [ADR-0011](ADR-0011.md) | Formatted-text exchange log format |
| [ADR-0012](ADR-0012.md) | Rust retained until parity cutover, then one-commit delete |

Source material for these records: `ARCHITECTURE_v2.md`, `GAPS.md`,
`AI_SLOP.md`, and the design-interview decision ledger, all under
`golang_port/docs/`.
