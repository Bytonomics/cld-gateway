---
type: decision
title: "ADR-0001: 1-to-n scope — Claude Code inbound only"
status: stable
tags:
  - adr
  - architecture
generated:
  by: claude-sonnet-5
  at: 2026-09-03T00:00:00Z
---

# ADR-0001: 1-to-n scope — Claude Code inbound only

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

The Rust implementation only ever received one kind of inbound client: Claude Code. Its
request normalization — command envelope parsing, slash-command promotion, directive
injection, the whole of `claude_code_context.rs` and `claude_code_inclusion.rs` — is written
against Claude Code's specific request shapes and conventions, not a generic "coding agent
client" abstraction. The outbound side is where real variability exists: today one backend
(Codex/ChatGPT), with a second backend a plausible near-term addition.

The question for the port: build a neutral intermediate representation now so both axes
(inbound harness, outbound backend) can vary independently, or keep the port's shape matched
to what's actually varying.

## Options Considered

### Option 1: Neutral IR across both axes

**Pros:**

- Both the inbound-harness axis and the outbound-backend axis could vary independently in
  the future without touching the other.
- Symmetric design — no axis is treated as more likely to grow than the other.

**Cons:**

- No second inbound harness exists to validate the abstraction against.
- Claude Code's request shape has enough special-cased behavior (slash commands, directive
  injection) that a premature abstraction would likely need to leak through anyway.

### Option 2: 1-to-n scope — Claude Code is the only inbound harness

**Pros:**

- Request normalization can stay concrete and Claude-Code-specific instead of routing
  through an abstraction with one implementation.
- Adding a second *backend* stays cheap: one new package under `core/impl/port/backend/`,
  one translator, one config entry, zero core edits.

**Cons:**

- Adding a second inbound harness later is real work, not a config flip.

## Decision

Scope is 1-to-n: Claude Code is the only inbound harness. No harness port, no neutral IR.
The extension axis is backends — many outbound providers, one active at a time, added by
implementing one interface and registering one config entry.

## Consequences

### Positive

- Request normalization can stay concrete and Claude-Code-specific instead of routing
  through an abstraction with one implementation.
- Adding a second *backend* stays cheap: one new package under `core/impl/port/backend/`,
  one translator, one config entry, zero core edits. See
  [Extending: backends](../how-to/extending-backends.md).

### Negative

- Adding a second inbound harness later is real work, not a config flip.

### Mitigation

- Accepted deliberately: no second harness exists yet, and speculative generality here
  would be paid for by every backend port along the way.

## References

- [ADR-0006: Backend port + extend-via-composition translators](ADR-0006-backend-port-composition-translators.md)
- `core/impl/port/backend/`
