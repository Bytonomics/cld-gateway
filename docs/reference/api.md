---
type: reference
title: "API Reference"
status: stable
tags:
  - api
  - http
stale_after: 2026-12-02
generated:
  by: claude-sonnet-5
  at: 2026-09-03T00:00:00Z
---

# API reference

| Section | What it covers |
|---------|----------------|
| [Routes](#routes) | The full route table |
| [Errors](#errors) | The shared error body shape and error codes |

The gateway exposes an Anthropic-compatible Messages API plus a small set
of operational endpoints. All routes are served from the address in
[Configuration](configuration/index.md) (`127.0.0.1:6473` packaged,
`127.0.0.1:6483` developer).

## Routes

| Method | Path | Purpose |
|---|---|---|
| `GET` | `/health` | Liveness check. |
| `GET` | `/auth/status` | Current backend credential status. |
| `POST` | `/auth/refresh` | Force a credential refresh. |
| `GET` | `/v1/models` | List models the active backend serves. |
| `POST` | `/v1/messages` | Send a conversation turn (unary or streamed). |
| `POST` | `/v1/messages/count_tokens` | Estimate token usage for a request without sending it. |

### `GET /health`

Returns `200` with a small JSON body once the process is up and its config
loaded successfully. Cheap enough to poll frequently — this is what the
Homebrew service uses to judge whether the process is alive.

### `GET /auth/status`

Reports whether stored backend credentials are present and currently
valid, without exposing the credentials themselves.

### `POST /auth/refresh`

Forces an immediate credential refresh against the backend, independent of
the automatic refresh the gateway already does as needed. Useful after
you suspect a token has gone stale outside the gateway's own tracking.

### `GET /v1/models`

Returns the models the currently active backend serves, in the shape a
Messages API client expects for model listing.

### `POST /v1/messages`

The main endpoint. Accepts an Anthropic Messages API request body —
`model`, `messages`, `system`, `tools`, `max_tokens`, and the rest of the
usual fields — and either:

- returns a single JSON response body (when `stream` is not set or `false`), or
- streams a Server-Sent Events response (when `stream: true`).

**Streaming event sequence.** A streamed response follows the same event
shape a client built against the Anthropic API already expects: a
`message_start` event opens the response, one or more `content_block_start`
/ `content_block_delta` / `content_block_stop` cycles carry the actual
content (text, tool-use blocks, or thinking, depending on what the model
produced), a `message_delta` event carries final usage and stop-reason
information, and a `message_stop` event closes the stream. Each event is
flushed to the client as soon as it's produced — the gateway does not
buffer a partial response before sending it.

### `POST /v1/messages/count_tokens`

Accepts the same request shape as `/v1/messages` but only estimates the
token count of the input; it does not contact the backend or produce a
completion. Useful for checking a request against a context limit before
sending it for real.

## Errors

Every error response — from any route — uses the same Anthropic-style
error shape:

```json
{
  "type": "error",
  "error": {
    "type": "<error_code>",
    "message": "<human-readable detail>"
  }
}
```

`<error_code>` is one of:

| Code | Meaning |
|---|---|
| `invalid_request_error` | The request body failed validation. |
| `authentication_error` | Missing or invalid backend credentials. |
| `permission_error` | The request isn't permitted (independent of auth). |
| `not_found_error` | The requested resource doesn't exist. |
| `rate_limit_error` | The backend (or the gateway) is rate-limiting. |
| `api_error` | An unexpected error while talking to the backend. |
| `overloaded_error` | The backend is temporarily overloaded. |
| `gateway_state_error` | The gateway's own conversation-state store hit a problem. |

For `invalid_request_error`, the message includes which field in the
request body failed and why, so you can fix the request without guessing.
