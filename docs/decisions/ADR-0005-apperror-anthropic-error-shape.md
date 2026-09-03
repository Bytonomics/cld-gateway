---
type: decision
title: "ADR-0005: AppError → Anthropic error shape"
status: stable
tags:
  - adr
  - error-handling
generated:
  by: claude-sonnet-5
  at: 2026-09-03T00:00:00Z
---

# ADR-0005: AppError → Anthropic error shape

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

Because the gateway's whole job is presenting an Anthropic-compatible surface, every error
it returns — validation failures, auth problems, backend failures, internal state errors —
has to come out as `{"type":"error","error":{"type":"<code>","message":"..."}}`, regardless
of where in the request-handling pipeline it originated. Scattering that serialization logic
across every handler risks drift: one handler formats it slightly differently, or forgets a
field, and now a client-visible contract is inconsistent.

## Options Considered

### Option 1: Per-handler error formatting

**Pros:**

- Each handler has full local control over its own error output.

**Cons:**

- This is exactly the drift risk described above: one handler formats slightly differently,
  or forgets a field, and the client-visible contract becomes inconsistent.

### Option 2: Single `AppError` type + central `HTTPErrorHandler`

**Pros:**

- One place to audit for wire-format correctness, one place to add a new error code if the
  closed set proves incomplete.
- The Rust implementation already proved a centralized error handler works for this shape
  of API.

**Cons:**

- Every internal error that should be client-visible has to be deliberately wrapped as an
  `AppError`, or it falls through to the recovery middleware's generic `api_error` fallback.

## Decision

A single `AppError` type (`Code`, `Message`, `HTTPStatus`, wrapped `Cause`) is the one error
type every layer of the request pipeline produces or wraps into. A central Echo
`HTTPErrorHandler` is the *only* place that serializes an error to the wire — handlers,
services, and adapters return or wrap `AppError` values (or plain errors that `errors.As`
can unwrap to one) and never write an error response themselves.

`Code` is a closed set: `invalid_request_error`, `authentication_error`, `permission_error`,
`not_found_error`, `rate_limit_error`, `api_error`, `overloaded_error`,
`gateway_state_error`. Pedantigo validation failures are mapped to `invalid_request_error`
with the field path folded into the message text.

## Consequences

### Positive

- One place to audit for wire-format correctness, one place to add a new error code if the
  set above proves incomplete.

### Negative

- Every internal error that should be client-visible has to be deliberately wrapped as an
  `AppError`; anything that isn't gets whatever the recovery middleware's generic fallback
  produces (an `api_error`).

### Mitigation

- Treated as a useful forcing function rather than a defect: an un-wrapped error reaching
  the handler boundary is itself a sign the error path wasn't thought through.

## References

- [ADR-0003: Echo + pedantigoecho + pedantigo v2](ADR-0003-echo-pedantigo.md)
- `middleware/recovery.go`
