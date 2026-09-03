---
type: decision
title: "ADR-0003: Echo + pedantigoecho + pedantigo v2"
status: stable
tags:
  - adr
  - http
generated:
  by: claude-sonnet-5
  at: 2026-09-03T00:00:00Z
---

# ADR-0003: Echo + pedantigoecho + pedantigo v2

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

The Rust HTTP surface (`gateway-http-anthropic`) is built on a stack of routing, request
binding, and schema validation. The Go port needs an equivalent: an HTTP router, a way to
bind request bodies into typed structs, and a validation layer that can turn a bad field
into a structured `invalid_request_error` with a field path in the message (see
[ADR-0005](ADR-0005-apperror-anthropic-error-shape.md)).

## Options Considered

### Option 1: `gin` / `chi` + hand-rolled validation

**Pros:**

- Widely used Go router ecosystem, large amount of existing documentation.

**Cons:**

- `pedantigo` and `pedantigoecho` already exist and are proven in `smritea-cloud`, so this
  option would mean re-solving "typed body binding with field-path validation errors" from
  scratch.

### Option 2: `echo` v4 + `pedantigoecho` v2 + `pedantigo` v2

**Pros:**

- `echo`'s central `HTTPErrorHandler` hook is exactly the seam needed to serialize every
  error into the Anthropic error shape in one place instead of at every handler.
- `pedantigo` and `pedantigoecho` are already proven in `smritea-cloud`, reusing a solved
  problem instead of re-solving it.

**Cons:**

- GET/DELETE routes take query parameters, which the binder doesn't cover; those structs
  need explicit validation after `c.Bind`.

## Decision

- **Router:** `echo` v4 — mature, minimal middleware surface, and the central
  `HTTPErrorHandler` hook it exposes is exactly the seam needed to serialize every error
  into the Anthropic error shape in one place instead of at every handler.
- **Binder:** `pedantigoecho` v2, installed as `e.Binder = pedantigoecho.NewBinder()`,
  covering POST/PUT/PATCH body binding.
- **Validation:** `pedantigo` v2's `validate` struct tag, with each request struct
  registered explicitly: `var _ = validator.Register(validator.New[T]())` (this panics at
  startup if a struct is malformed, which is the intended fail-fast behavior — a broken
  validator registration should never reach production).

GET/DELETE routes take query parameters, which the binder doesn't cover; those structs are
validated explicitly after `c.Bind`.

## Consequences

### Positive

- Validation errors have one code path to the wire format, not one per handler — reduces
  the chance of a handler forgetting to map a bind failure into the Anthropic shape.

### Negative

- Every request DTO needs its registration line, or validation silently no-ops for that
  type until the panic is hit at first use.

### Mitigation

- Flag the registration-line requirement in code review for new DTOs — this is a sharp
  edge worth calling out explicitly rather than relying on the panic to catch it.

## References

- [ADR-0005: AppError → Anthropic error shape](ADR-0005-apperror-anthropic-error-shape.md)
- `handlers/`
