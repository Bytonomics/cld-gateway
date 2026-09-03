---
type: decision
title: "ADR-0006: Backend port + extend-via-composition translators"
status: stable
tags:
  - adr
  - architecture
generated:
  by: claude-sonnet-5
  at: 2026-09-03T00:00:00Z
---

# ADR-0006: Backend port + extend-via-composition translators

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

Two related extensibility questions: how does a new outbound backend plug in (see
[ADR-0001](ADR-0001-claude-code-inbound-only.md) for why this is the axis that needs to
flex), and how does a backend's request/response translation logic share code with other
backends without duplicating the parts that are the same across all of them
(system-prompt-to-instructions assembly, message shaping, tool-schema gating, output-config
mapping)?

## Options Considered

### Option 1: One translator implementation per backend, no shared base

**Pros:**

- Each backend's translator is fully independent, no shared abstraction to get wrong.

**Cons:**

- Every backend needs the same system→instructions assembly and tool-schema gating;
  duplicating it defeats the purpose of adding a second backend cheaply.

### Option 2: Backend port + extend-via-composition translators

**Pros:**

- A second backend is: one new package under `core/impl/port/backend/` implementing
  `Backend`, one translator embedding `GenericBackendTranslator`, one config entry. Zero
  edits to `core/domain`.
- Shared translation logic lives in exactly one place (`GenericBackendTranslator`), so a
  bug fix or policy change there applies to every backend without hunting down duplicated
  copies.

**Cons:**

- Requires Go's method-promotion + compile-time interface assertion pattern to be
  understood correctly by anyone adding a backend, or an override's signature can silently
  drift from the interface.

## Decision

**Backend port.** A `Backend` interface (`SendUnary`, `SendStream`, `Capabilities`,
`EvictSession`, `HasLiveSession`, `LiveChainID`) is the entire contract a new backend needs
to satisfy. `Capabilities` reports what the backend can do (`WebSocketDelta`,
`ServerSideState`) so core orchestration logic can adapt without knowing the backend's
identity.

**Translator via composition.** `BackendTranslator` is the interface (`TranslateRequest`,
`TranslateResponseEvent`, `BuildUnaryResponse`). `GenericBackendTranslator` implements the
shared policy every backend needs. A concrete backend's translator (for example
`OpenAITranslator`) embeds a `*GenericBackendTranslator` by pointer and overrides only the
methods that differ for that backend — Go's method promotion means the concrete type
satisfies `BackendTranslator` automatically for everything it doesn't override, and a
compile-time assertion (`var _ translator.BackendTranslator = (*OpenAITranslator)(nil)`)
catches it immediately if an override's signature drifts from the interface.

## Consequences

### Positive

- A second backend is: one new package under `core/impl/port/backend/` implementing
  `Backend`, one translator embedding `GenericBackendTranslator`, one config entry. Zero
  edits to `core/domain`. See [Extending: backends](../how-to/extending-backends.md) and
  [Extending: translators](../how-to/extending-translators.md).
- Shared translation logic lives in exactly one place (`GenericBackendTranslator`), so a
  bug fix or policy change there applies to every backend without hunting down duplicated
  copies.

### Negative

- This is composition standing in for inheritance — anyone unfamiliar with Go's embedding
  idiom needs to learn it to safely extend a translator.

### Mitigation

- Documented here and in the how-to guides above; the Rust source used a comparable
  extend-and-override pattern, so this is the direct idiomatic Go equivalent, not a
  workaround.

## References

- [ADR-0001: 1-to-n scope — Claude Code inbound only](ADR-0001-claude-code-inbound-only.md)
- [Extending: backends](../how-to/extending-backends.md)
- [Extending: translators](../how-to/extending-translators.md)
